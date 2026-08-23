//! Pipeline de integración Zero-Copy: Processor -> Routing (EAT) -> RingBuffer Storage.

use crate::parser::primary_block::ParsedBundleHeader;
use crate::processor::fragmentation::ReassemblySlot;
use crate::processor::state_machine::{BundleProcessor, ProcessStatus};
use crate::routing::interval_tree::CgrIntervalTree;
use crate::storage::ring_buffer::Producer;
use crate::telemetry::metrics::SystemMetrics;

/// Acción resultante del análisis de enrutamiento.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteDecision {
    ForwardNextHop { node_id: u64, start_time: u64 },
    StoreLocal,
    DropNoRoute,
    DropExpiredOrMalformed,
    PendingReassembly,
}

/// Pipeline de integración de alto rendimiento (Hot-Path).
pub struct IngestionPipeline;

impl IngestionPipeline {
    /// Ingesta un raw bundle, lo procesa, reensambla si es fragmento y busca la mejor ventana EAT.
    pub fn ingest_and_route<const CAP: usize, const RING_SIZE: usize, const REASSEMBLY_BUF: usize>(
        raw_bytes: &[u8],
        current_ts: u64,
        router: &CgrIntervalTree<CAP>,
        producer: &mut Producer<'_, RING_SIZE>,
        reassembly_slot: Option<&mut ReassemblySlot<REASSEMBLY_BUF>>,
        metrics: Option<&SystemMetrics>,
    ) -> RouteDecision {
        // 1. Decodificación y validación con la máquina de estados
        let process_res = BundleProcessor::process_bundle(raw_bytes, current_ts, metrics);

        if process_res.status != ProcessStatus::Accepted {
            return RouteDecision::DropExpiredOrMalformed;
        }

        let header: ParsedBundleHeader<'_> = match process_res.primary_header {
            Some(h) => h,
            None => return RouteDecision::DropExpiredOrMalformed,
        };

        // 2. Control de fragmentación BPv7 (Bit 0 de processing_flags indica Bundle Is A Fragment)
        let is_fragment = (header.processing_flags & 0x01) != 0;
        if is_fragment {
            if let Some(slot) = reassembly_slot {
                // Usamos 0 como offset por defecto hasta extraer el payload block específico
                let offset = 0;
                let total_len = raw_bytes.len() as u64;

                let is_complete = match slot.insert_fragment(offset, total_len, raw_bytes) {
                    Ok(complete) => complete,
                    Err(_) => {
                        if let Some(m) = metrics {
                            m.record_drop();
                        }
                        return RouteDecision::DropExpiredOrMalformed;
                    }
                };

                if !is_complete {
                    return RouteDecision::PendingReassembly;
                }
            }
        }

        // 3. Consulta de ventana de contacto en CgrIntervalTree (O(log N))
        let decision = match router.find_next(current_ts) {
            Some(interval) => RouteDecision::ForwardNextHop {
                node_id: interval.target_node_id as u64,
                start_time: interval.start_time,
            },
            None => RouteDecision::DropNoRoute,
        };

        // 4. Encolado lock-free via Productor SPSC
        if let RouteDecision::ForwardNextHop { node_id, .. } = decision {
            let bytes = node_id.to_le_bytes();
            if producer.push(&bytes).is_err() {
                if let Some(m) = metrics {
                    m.record_drop();
                }
            }
        }

        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::interval_tree::ContactInterval;
    use crate::storage::ring_buffer::LockFreeRingBuffer;

    fn make_valid_primary() -> &'static [u8] {
        &[
            0xA6,
            0x01, 0x07,
            0x02, 0x03,
            0x04, 0x64, b'd', b'e', b's', b't',
            0x05, 0x63, b's', b'r', b'c',
            0x06, 0x82, 0x19, 0x04, 0xD2, 0x01, // creation_ts = 1234
            0x07, 0x19, 0x0E, 0x10,             // lifetime = 3600
        ]
    }

    #[test]
    fn test_pipeline_ingest_route_and_enqueue() {
        let mut router = CgrIntervalTree::<16>::new();
        router
            .insert(ContactInterval {
                start_time: 2000,
                end_time: 3000,
                target_node_id: 42,
            })
            .unwrap();

        let mut ring = LockFreeRingBuffer::<64>::new();
        let (mut producer, mut consumer) = ring.split();
        let metrics = SystemMetrics::new();
        let raw_bundle = make_valid_primary();

        let decision = IngestionPipeline::ingest_and_route::<16, 64, 1024>(
            raw_bundle,
            1500,
            &router,
            &mut producer,
            None,
            Some(&metrics),
        );

        assert_eq!(
            decision,
            RouteDecision::ForwardNextHop {
                node_id: 42,
                start_time: 2000
            }
        );

        let mut buf = [0u8; 8];
        let read = consumer.pop(&mut buf);
        assert_eq!(read, 8);
        assert_eq!(u64::from_le_bytes(buf), 42);
    }
}