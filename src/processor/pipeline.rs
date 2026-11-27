//! Zero-copy ingestion pipeline: processor → reassembly → CGR routing → queue/WAL.
//!
//! The pipeline is the hot path of a DTN node. It performs no allocation: the
//! caller supplies the router, SPSC producer, optional reassembly slot,
//! optional write-ahead log and optional metrics object.
//!
//! ```
//! use dtn_core::processor::pipeline::{IngestionPipeline, RouteDecision};
//! use dtn_core::routing::interval_tree::{CgrIntervalTree, ContactInterval};
//! use dtn_core::storage::ring_buffer::LockFreeRingBuffer;
//!
//! let bundle = [
//!     0x9f, 0x88, 0x07, 0x00, 0x00,
//!     0x82, 0x01, 0x00, 0x82, 0x01, 0x00, 0x82, 0x01, 0x00,
//!     0x82, 0x00, 0x01, 0x19, 0xea, 0x60,
//!     0x85, 0x01, 0x01, 0x00, 0x00, 0x41, 0x58, 0xff,
//! ];
//! let mut router = CgrIntervalTree::<4>::new();
//! let _ = router.insert(ContactInterval::new(0, 10_000, 42));
//! let mut ring = LockFreeRingBuffer::<64>::new();
//! let (mut producer, _) = ring.split();
//! let decision = IngestionPipeline::ingest_and_route::<4, 64, 256, 2>(
//!     &bundle, 1_000, &router, &mut producer, None, None, None,
//! );
//! assert!(matches!(decision, RouteDecision::ForwardNextHop { node_id: 42, .. }));
//! ```

use crate::parser::primary_block::ParsedBundleHeader;
use crate::processor::fragmentation::ReassemblySlot;
use crate::processor::state_machine::{BundleProcessor, ProcessStatus};
use crate::routing::interval_tree::CgrIntervalTree;
use crate::storage::ring_buffer::Producer;
use crate::storage::wal::DirectWal;
use crate::telemetry::metrics::SystemMetrics;

/// Action selected by the ingestion pipeline for one bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteDecision {
    /// A contact is available now (or at the reported start time).
    ForwardNextHop {
        /// Target node identifier from the contact plan.
        node_id: u64,
        /// Earliest expected arrival time (DTN milliseconds).
        start_time: u64,
    },
    /// Bundle is stored locally (reserved for application delivery).
    StoreLocal,
    /// Queue was full; the bundle was persisted in the WAL instead.
    PersistedToWal,
    /// No active contact and no WAL capacity: bundle was dropped.
    DropNoRoute,
    /// State machine rejected the bundle (malformed, expired, hop/age limit).
    DropExpiredOrMalformed,
    /// BPSec BIB was missing or its integrity result did not verify.
    DropIntegrityFailed,
    /// Fragment was accepted but the ADU is not complete yet.
    PendingReassembly,
}

/// Stateless high-throughput ingestion pipeline.
pub struct IngestionPipeline;

impl IngestionPipeline {
    /// Ingests a raw bundle without BPSec verification.
    ///
    /// Equivalent to [`Self::ingest_and_route_with_security`] with no verifier;
    /// retained for callers that do not use BPSec.
    #[allow(clippy::too_many_arguments)]
    pub fn ingest_and_route<
        const CAP: usize,
        const RING_SIZE: usize,
        const REASSEMBLY_BUF: usize,
        const WAL_PAGES: usize,
    >(
        raw_bytes: &[u8],
        current_ts: u64,
        router: &CgrIntervalTree<CAP>,
        producer: &mut Producer<'_, RING_SIZE>,
        reassembly_slot: Option<&mut ReassemblySlot<REASSEMBLY_BUF>>,
        wal: Option<&mut DirectWal<WAL_PAGES>>,
        metrics: Option<&SystemMetrics>,
    ) -> RouteDecision {
        Self::ingest_and_route_with_security::<CAP, RING_SIZE, REASSEMBLY_BUF, WAL_PAGES>(
            raw_bytes,
            current_ts,
            router,
            producer,
            reassembly_slot,
            wal,
            metrics,
            None,
        )
    }

    /// Ingests a raw bundle, validates RFC 9171 and optional BPSec BIB
    /// integrity, reassembles / routes it and enqueues it (or falls back to
    /// WAL).
    ///
    /// A BIB that is missing (when required) or that fails verification causes
    /// [`RouteDecision::DropIntegrityFailed`] **before** any queue or WAL
    /// insertion. `CAP`, `RING_SIZE`, `REASSEMBLY_BUF` and `WAL_PAGES` are
    /// compile-time capacities, so the whole operation is $O(1)$ in SRAM.
    #[allow(clippy::too_many_arguments)]
    pub fn ingest_and_route_with_security<
        const CAP: usize,
        const RING_SIZE: usize,
        const REASSEMBLY_BUF: usize,
        const WAL_PAGES: usize,
    >(
        raw_bytes: &[u8],
        current_ts: u64,
        router: &CgrIntervalTree<CAP>,
        producer: &mut Producer<'_, RING_SIZE>,
        reassembly_slot: Option<&mut ReassemblySlot<REASSEMBLY_BUF>>,
        wal: Option<&mut DirectWal<WAL_PAGES>>,
        metrics: Option<&SystemMetrics>,
        security: Option<&crate::bpsec::BibVerifier>,
    ) -> RouteDecision {
        let process_res = BundleProcessor::process_bundle(raw_bytes, current_ts, metrics);
        if process_res.status != ProcessStatus::Accepted {
            return RouteDecision::DropExpiredOrMalformed;
        }
        let header: ParsedBundleHeader<'_> = match process_res.primary_header {
            Some(header) => header,
            None => return RouteDecision::DropExpiredOrMalformed,
        };
        let payload = match process_res.payload {
            Some(payload) => payload,
            None => return RouteDecision::DropExpiredOrMalformed,
        };

        // Integrity gate: nothing with a bad (or missing, when required) BIB
        // reaches the SPSC queue or the WAL.
        if let Some(verifier) = security {
            if verifier.verify(raw_bytes, &header, payload).is_err() {
                if let Some(m) = metrics {
                    m.record_drop();
                }
                return RouteDecision::DropIntegrityFailed;
            }
        }

        if header.is_fragment() {
            let slot = match reassembly_slot {
                Some(slot) => slot,
                None => return RouteDecision::DropExpiredOrMalformed,
            };
            let fragment = match process_res.payload {
                Some(payload) => payload,
                None => return RouteDecision::DropExpiredOrMalformed,
            };
            let offset = header.fragment_offset.unwrap_or(0);
            let total_len = match header.total_app_data_len {
                Some(total) => total,
                None => fragment.len() as u64,
            };
            match slot.insert_fragment(offset, total_len, fragment) {
                Ok(true) => {}
                Ok(false) => return RouteDecision::PendingReassembly,
                Err(_) => {
                    if let Some(m) = metrics {
                        m.record_drop();
                    }
                    return RouteDecision::DropExpiredOrMalformed;
                }
            }
        }

        let decision = match router.find_next(current_ts) {
            Some(interval) => RouteDecision::ForwardNextHop {
                node_id: u64::from(interval.target_node_id),
                start_time: interval.start_time,
            },
            None => RouteDecision::DropNoRoute,
        };

        match decision {
            RouteDecision::ForwardNextHop { node_id, .. } => {
                let bytes = node_id.to_le_bytes();
                if producer.push(&bytes).is_err() {
                    if let Some(w) = wal {
                        if w.append(raw_bytes).is_ok() {
                            return RouteDecision::PersistedToWal;
                        }
                    }
                    if let Some(m) = metrics {
                        m.record_drop();
                    }
                }
            }
            RouteDecision::DropNoRoute => {
                if let Some(w) = wal {
                    if w.append(raw_bytes).is_ok() {
                        return RouteDecision::PersistedToWal;
                    }
                }
            }
            _ => {}
        }
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::processor::fragmentation::ReassemblySlot;
    use crate::routing::interval_tree::ContactInterval;
    use crate::storage::ring_buffer::LockFreeRingBuffer;

    fn bundle() -> [u8; 32] {
        // RFC array primary block + payload block + break.
        [
            0x9f, 0x88, 0x07, 0x00, 0x00,
            0x82, 0x01, 0x00, 0x82, 0x01, 0x00, 0x82, 0x01, 0x00,
            0x82, 0x00, 0x01, 0x19, 0xea, 0x60,
            0x85, 0x01, 0x01, 0x00, 0x00, 0x45, b'h', b'e', b'l', b'l', b'o', 0xff,
        ]
    }

    fn legacy_bundle() -> &'static [u8] {
        &[
            0xa6, 0x01, 0x07, 0x02, 0x03, 0x04, 0x64, b'd', b'e', b's', b't',
            0x05, 0x63, b's', b'r', b'c', 0x06, 0x82, 0x19, 0x04, 0xd2, 0x01,
            0x07, 0x19, 0x0e, 0x10, 0x85, 0x01, 0x01, 0x00, 0x00, 0x45,
            b'h', b'e', b'l', b'l', b'o',
        ]
    }

    #[test]
    fn ingest_route_and_enqueue() {
        let mut router = CgrIntervalTree::<16>::new();
        assert!(router
            .insert(ContactInterval::new(2000, 3000, 42))
            .is_ok());
        let mut ring = LockFreeRingBuffer::<64>::new();
        let (mut producer, mut consumer) = ring.split();
        let mut slot = ReassemblySlot::<1024>::new();
        let metrics = SystemMetrics::new();

        let decision = IngestionPipeline::ingest_and_route::<16, 64, 1024, 2>(
            legacy_bundle(), 1500, &router, &mut producer, Some(&mut slot), None, Some(&metrics),
        );
        assert_eq!(
            decision,
            RouteDecision::ForwardNextHop { node_id: 42, start_time: 2000 }
        );
        let mut out = [0u8; 8];
        assert_eq!(consumer.pop(&mut out), 8);
        assert_eq!(u64::from_le_bytes(out), 42);
    }

    #[test]
    fn fallback_to_wal_when_no_route() {
        let router = CgrIntervalTree::<16>::new();
        let mut ring = LockFreeRingBuffer::<64>::new();
        let (mut producer, _) = ring.split();
        let mut wal = DirectWal::<2>::new();
        let bundle = bundle();
        let decision = IngestionPipeline::ingest_and_route::<16, 64, 1024, 2>(
            &bundle, 1500, &router, &mut producer, None, Some(&mut wal), None,
        );
        assert_eq!(decision, RouteDecision::PersistedToWal);
        assert!(wal.remaining() < DirectWal::<2>::capacity_bytes());
    }

    #[test]
    fn fragment_uses_payload_bytes_not_whole_bundle() {
        // Fragment flags=1, offset=2, total ADU length=8, payload "5678".
        let fragment = [
            0x9f, 0x8a, 0x07, 0x01, 0x00,
            0x82, 0x01, 0x00, 0x82, 0x01, 0x00, 0x82, 0x01, 0x00,
            0x82, 0x00, 0x01, 0x19, 0xea, 0x60,
            0x02, 0x08,
            0x85, 0x01, 0x01, 0x00, 0x00, 0x44, b'5', b'6', b'7', b'8', 0xff,
        ];
        let mut router = CgrIntervalTree::<4>::new();
        assert!(router.insert(ContactInterval::new(0, 10_000, 7)).is_ok());
        let mut ring = LockFreeRingBuffer::<64>::new();
        let (mut producer, _) = ring.split();
        let mut slot = ReassemblySlot::<32>::new();
        assert_eq!(
            IngestionPipeline::ingest_and_route::<4, 64, 32, 2>(
                &fragment, 1500, &router, &mut producer, Some(&mut slot), None, None,
            ),
            RouteDecision::PendingReassembly
        );
        assert_eq!(slot.bytes_received, 4);
        assert_eq!(&slot.payload_buffer[2..6], b"5678");
    }

    #[cfg(feature = "bpsec")]
    #[test]
    fn bpsec_gate_rejects_tampered_bundle_before_enqueue() {
        use crate::bpsec::{BibScope, BibSigner, BibVerifier, SecurityContext};
        use crate::storage::wal::{read_page, PAGE_SIZE};

        let key = [0x5au8; 32];
        let signer = BibSigner::new(key, SecurityContext::HmacSha256, BibScope::PrimaryAndPayload);
        let mut signed = [0u8; 128];
        let len = signer.attach(&bundle(), &mut signed).unwrap();

        let router = CgrIntervalTree::<4>::new(); // no contact -> WAL fallback
        let verifier = BibVerifier::required(key);

        let mut ring = LockFreeRingBuffer::<8>::new();
        let (mut producer, mut consumer) = ring.split();
        let mut wal = DirectWal::<2>::new();
        assert_eq!(
            IngestionPipeline::ingest_and_route_with_security::<4, 8, 64, 2>(
                &signed[..len], 1_500, &router, &mut producer, None, Some(&mut wal), None,
                Some(&verifier),
            ),
            RouteDecision::PersistedToWal
        );
        assert_eq!(wal.full_pages(), 1);

        let mut tampered = signed;
        tampered[len - 2] ^= 0x01; // payload byte after the BIB
        let mut ring2 = LockFreeRingBuffer::<8>::new();
        let (mut producer2, mut consumer2) = ring2.split();
        let mut wal2 = DirectWal::<2>::new();
        assert_eq!(
            IngestionPipeline::ingest_and_route_with_security::<4, 8, 64, 2>(
                &tampered[..len], 1_500, &router, &mut producer2, None, Some(&mut wal2), None,
                Some(&verifier),
            ),
            RouteDecision::DropIntegrityFailed
        );
        assert_eq!(wal2.full_pages(), 0); // never persisted
        let mut out = [0u8; 8];
        assert_eq!(consumer2.pop(&mut out), 0); // never enqueued

        // The accepted record is still a valid, CRC-sealed WAL page.
        let mut page = [0u8; PAGE_SIZE];
        assert!(wal.write_first_sealed_page(&mut page));
        assert_eq!(read_page(&page).unwrap().1.len(), len);
        let _ = (&mut consumer, &mut consumer2);
    }
}
