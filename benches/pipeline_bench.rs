use criterion::{black_box, criterion_group, criterion_main, Criterion};
use dtn_core::cla::udp::UdpFrameBuffer;
use dtn_core::routing::interval_tree::{CgrIntervalTree, ContactInterval};
use dtn_core::storage::ring_buffer::LockFreeRingBuffer;

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

fn bench_ingestion_pipeline_hotpath(c: &mut Criterion) {
    let raw_bundle = make_valid_primary();

    // Setup del entorno
    let mut router = CgrIntervalTree::<64>::new();
    router
        .insert(ContactInterval {
            start_time: 1000,
            end_time: 5000,
            target_node_id: 1001,
        })
        .unwrap();

    let mut ring = LockFreeRingBuffer::<4096>::new();
    let (mut producer, mut consumer) = ring.split();
    let mut cla_buf = UdpFrameBuffer::<1500>::new();

    c.bench_function("e2e_udp_to_pipeline_hotpath", |b| {
        b.iter(|| {
            // 1. Ingesta CLA
            cla_buf
                .ingest_raw_packet(black_box(raw_bundle))
                .unwrap();

            // 2. Procesamiento, enrutamiento CGR y encolado RingBuffer
            let decision = cla_buf.process_to_pipeline::<64, 4096, 1024, 4>(
                black_box(1500),
                black_box(&router),
                black_box(&mut producer),
                None,
                None,
            );

            // Evitar saturación del ring limpiando la cola para medición continua
            let mut tmp = [0u8; 8];
            let _ = consumer.pop(&mut tmp);

            black_box(decision);
        });
    });
}

criterion_group!(benches, bench_ingestion_pipeline_hotpath);
criterion_main!(benches);
