use criterion::{criterion_group, criterion_main, Criterion, Throughput, BatchSize};
use dtn_core::storage::ring_buffer::LockFreeRingBuffer;
use std::hint::black_box;

fn generate_mock_bundle_payload() -> Vec<u8> {
    let mut payload = vec![0x9f, 0x07, 0x00, 0x00];
    payload.extend(vec![0x42; 1024]);
    payload.push(0xff);
    payload
}

fn bench_full_ingest_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("Daemon_Core_Ingest_Hotpath");
    let payload = generate_mock_bundle_payload();

    group.throughput(Throughput::Bytes(payload.len() as u64));

    group.bench_function("ingest_hotpath_throughput", |b| {
        b.iter_batched(
            || {
                let ring = LockFreeRingBuffer::<2048>::new();
                let out_buf = vec![0u8; 2048];
                (ring, payload.clone(), out_buf)
            },
            |(mut ring, data, mut out_buf)| {
                let (mut producer, mut consumer) = ring.split();
                let _ = black_box(producer.push(&data));
                let bytes_read = black_box(consumer.pop(&mut out_buf));
                black_box(bytes_read);
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_full_ingest_pipeline);
criterion_main!(benches);