//! Convergence Layer Adapter (CLA) para UDP en entorno no_std.

use crate::processor::pipeline::{IngestionPipeline, RouteDecision};
use crate::routing::interval_tree::CgrIntervalTree;
use crate::storage::ring_buffer::Producer;
use crate::storage::wal::DirectWal;
use crate::telemetry::metrics::SystemMetrics;

/// Búfer de recepción no bloqueante para tramas de red UDP.
pub struct UdpFrameBuffer<const MAX_FRAME_SIZE: usize> {
    storage: [u8; MAX_FRAME_SIZE],
    len: usize,
}

impl<const MAX_FRAME_SIZE: usize> UdpFrameBuffer<MAX_FRAME_SIZE> {
    pub const fn new() -> Self {
        Self {
            storage: [0u8; MAX_FRAME_SIZE],
            len: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Recibe un payload crudo proveniente de la interfaz de red (e.g. recvfrom).
    pub fn ingest_raw_packet(&mut self, bytes: &[u8]) -> Result<&[u8], &'static str> {
        if bytes.len() > MAX_FRAME_SIZE {
            return Err("packet size exceeds CLA buffer capacity");
        }
        self.storage[..bytes.len()].copy_from_slice(bytes);
        self.len = bytes.len();
        Ok(&self.storage[..self.len])
    }

    pub fn payload(&self) -> &[u8] {
        &self.storage[..self.len]
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Dispara la ingesta directamente hacia la canalización de enrutamiento del núcleo DTN.
    pub fn process_to_pipeline<
        const CAP: usize,
        const RING_SIZE: usize,
        const REASSEMBLY_BUF: usize,
        const WAL_PAGES: usize,
    >(
        &self,
        current_ts: u64,
        router: &CgrIntervalTree<CAP>,
        producer: &mut Producer<'_, RING_SIZE>,
        wal: Option<&mut DirectWal<WAL_PAGES>>,
        metrics: Option<&SystemMetrics>,
    ) -> RouteDecision {
        IngestionPipeline::ingest_and_route::<CAP, RING_SIZE, REASSEMBLY_BUF, WAL_PAGES>(
            self.payload(),
            current_ts,
            router,
            producer,
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
    fn test_udp_cla_ingest() {
        let mut cla = UdpFrameBuffer::<1500>::new();
        let frame = b"UDP_DTN_PAYLOAD_TEST";

        let rx = cla.ingest_raw_packet(frame).unwrap();
        assert_eq!(rx, frame);
        assert_eq!(cla.payload(), frame);
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

        let mut ring = LockFreeRingBuffer::<64>::new();
        let (mut producer, mut consumer) = ring.split();

        let decision = cla.process_to_pipeline::<16, 64, 1024, 2>(
            1500,
            &router,
            &mut producer,
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

        let mut buf = [0u8; 8];
        let read = consumer.pop(&mut buf);
        assert_eq!(read, 8);
        assert_eq!(u64::from_le_bytes(buf), 100);
    }
}