use dtn_core::parser::primary_block::parse_primary_block;
use dtn_core::storage::ring_buffer::LockFreeRingBuffer;
use dtn_core::storage::wal::{DirectWal, WalWriter};

struct MockDiskWriter {
    pub written_pages: usize,
}

impl WalWriter for MockDiskWriter {
    fn write_page(&mut self, _page: &[u8]) -> Result<(), &'static str> {
        self.written_pages += 1;
        Ok(())
    }
}

#[test]
fn stress_cbor_ring_wal_pipeline() {
    const ITERATIONS: usize = 50_000;
    
    let valid_cbor_header = [
        0xA6,
        0x01, 0x07,
        0x02, 0x03,
        0x04, 0x64, b'd', b'e', b's', b't',
        0x05, 0x63, b's', b'r', b'c',
        0x06, 0x82, 0x19, 0x04, 0xD2, 0x01,
        0x07, 0x19, 0x0E, 0x10,
    ];

    let mut rb: LockFreeRingBuffer<8192> = LockFreeRingBuffer::new();
    let (mut prod, mut cons) = rb.split();
    let mut wal: DirectWal<4> = DirectWal::new();
    let mut disk = MockDiskWriter { written_pages: 0 };

    let mut read_buf = [0u8; 64];

    for _ in 0..ITERATIONS {
        let parsed = parse_primary_block(&valid_cbor_header).expect("CBOR parse failure");
        assert_eq!(parsed.version, 7);

        prod.push(parsed.raw).expect("RingBuffer Push failed");

        let popped = cons.pop(&mut read_buf);
        assert_eq!(popped, valid_cbor_header.len());

        wal.append(&read_buf[..popped]).expect("WAL Append failed");

        if wal.full_pages() > 0 {
            wal.flush_pages(&mut disk).expect("WAL Flush failed");
        }
    }

    if wal.full_pages() > 0 {
        wal.flush_pages(&mut disk).expect("WAL Final Flush failed");
    }

    assert!(disk.written_pages > 0, "No se escribieron páginas en el disco simulado");
}