//! UDP convergence-layer adapter for `no_std` network stacks (e.g. `smoltcp`).
//!
//! The adapter owns one fixed-size RX frame buffer. Datagrams are copied into
//! it once (there is no socket API in `core`), after which all DTN parsing and
//! routing is zero-copy. No heap allocator is used anywhere in the path.
//!
//! ```
//! use dtn_core::cla::udp::UdpFrameBuffer;
//! let mut cla = UdpFrameBuffer::<64>::new();
//! let stored = cla.ingest_raw_packet(b"bundle").unwrap();
//! assert_eq!(stored, b"bundle");
//! assert_eq!(cla.payload(), b"bundle");
//! ```

use crate::error::{DtnError, DtnResult};
use crate::processor::pipeline::{IngestionPipeline, RouteDecision};
use crate::routing::interval_tree::CgrIntervalTree;
use crate::storage::outbound::OutboundBundleQueue;
use crate::storage::wal::DirectWal;
use crate::telemetry::metrics::SystemMetrics;

/// Non-blocking receive buffer for UDP datagrams.
///
/// `MAX_FRAME_SIZE` should match the MTU of the convergence layer. Oversized
/// datagrams are rejected atomically with [`DtnError::BufferTooSmall`]; the
/// previous frame remains available in [`Self::payload`].
pub struct UdpFrameBuffer<const MAX_FRAME_SIZE: usize> {
    storage: [u8; MAX_FRAME_SIZE],
    len: usize,
}

impl<const MAX_FRAME_SIZE: usize> UdpFrameBuffer<MAX_FRAME_SIZE> {
    /// Creates an empty frame buffer in `const` context.
    pub const fn new() -> Self {
        Self {
            storage: [0u8; MAX_FRAME_SIZE],
            len: 0,
        }
    }

    /// Returns `true` when no datagram has been ingested.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Number of bytes in the stored datagram.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Maximum datagram size accepted by this buffer.
    pub const fn capacity(&self) -> usize {
        MAX_FRAME_SIZE
    }

    /// Copies a datagram received from the network stack into the CLA buffer.
    pub fn ingest_raw_packet(&mut self, bytes: &[u8]) -> DtnResult<&[u8]> {
        if bytes.len() > MAX_FRAME_SIZE {
            return Err(DtnError::BufferTooSmall);
        }
        self.storage[..bytes.len()].copy_from_slice(bytes);
        self.len = bytes.len();
        Ok(&self.storage[..self.len])
    }

    /// Borrows the stored datagram.
    pub fn payload(&self) -> &[u8] {
        &self.storage[..self.len]
    }

    /// Discards the stored datagram.
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Feeds the stored datagram directly into the ingestion pipeline.
    pub fn process_to_pipeline<
        const CAP: usize,
        const OUT_SLOTS: usize,
        const MAX_BUNDLE_SIZE: usize,
        const REASSEMBLY_BUF: usize,
        const WAL_PAGES: usize,
    >(
        &self,
        current_ts: u64,
        router: &CgrIntervalTree<CAP>,
        outbound: &mut OutboundBundleQueue<OUT_SLOTS, MAX_BUNDLE_SIZE>,
        wal: Option<&mut DirectWal<WAL_PAGES>>,
        metrics: Option<&SystemMetrics>,
    ) -> RouteDecision {
        IngestionPipeline::ingest_and_route::<
            CAP,
            OUT_SLOTS,
            MAX_BUNDLE_SIZE,
            REASSEMBLY_BUF,
            WAL_PAGES,
        >(
            self.payload(),
            current_ts,
            router,
            outbound,
            None,
            wal,
            metrics,
        )
    }
}

impl<const MAX_FRAME_SIZE: usize> Default for UdpFrameBuffer<MAX_FRAME_SIZE> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::interval_tree::ContactInterval;
    use crate::storage::outbound::OutboundBundleQueue;

    fn make_valid_primary() -> &'static [u8] {
        &[
            0xA6,
            0x01, 0x07,
            0x02, 0x00,
            0x04, 0x64, b'd', b'e', b's', b't',
            0x05, 0x63, b's', b'r', b'c',
            0x06, 0x82, 0x19, 0x04, 0xD2, 0x01,
            0x07, 0x19, 0x0E, 0x10,
            0x85,
            0x01,
            0x01,
            0x00,
            0x00,
            0x45,
            b'h', b'e', b'l', b'l', b'o',
        ]
    }

    #[test]
    fn test_udp_cla_ingest() {
        let mut cla = UdpFrameBuffer::<1500>::new();
        let frame = b"UDP_DTN_PAYLOAD_TEST";

        let rx = cla.ingest_raw_packet(frame).unwrap();
        assert_eq!(rx, frame);
        assert_eq!(cla.payload(), frame);
        assert_eq!(cla.len(), frame.len());
        assert!(!cla.is_empty());
        cla.clear();
        assert!(cla.is_empty());
    }

    #[test]
    fn oversized_frame_is_rejected_atomically() {
        let mut cla = UdpFrameBuffer::<4>::new();
        cla.ingest_raw_packet(b"keep").unwrap();
        assert_eq!(cla.ingest_raw_packet(b"too long"), Err(DtnError::BufferTooSmall));
        assert_eq!(cla.payload(), b"keep");
    }

    #[test]
    fn test_udp_cla_to_pipeline_flow() {
        let mut cla = UdpFrameBuffer::<1500>::new();
        let frame = make_valid_primary();

        cla.ingest_raw_packet(frame).unwrap();

        let mut router = CgrIntervalTree::<16>::new();
        router
            .insert(ContactInterval {
                start_time: 2000,
                end_time: 3000,
                target_node_id: 100,
            })
            .unwrap();

        let mut outbound = OutboundBundleQueue::<4, 128>::new();

        let decision = cla.process_to_pipeline::<16, 4, 128, 1024, 2>(
            1500,
            &router,
            &mut outbound,
            None,
            None,
        );

        assert_eq!(
            decision,
            RouteDecision::ForwardNextHop {
                node_id: 100,
                start_time: 2000
            }
        );

        // Simulate the firmware's verified persistence before forwarding.
        outbound.mark_last_persisted().unwrap();
        let (next_hop, forwarded) = outbound.front_parts().unwrap();
        assert_eq!(next_hop, 100);
        assert_eq!(forwarded, frame);
    }
}
