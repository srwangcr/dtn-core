use criterion::{criterion_group, criterion_main, Criterion};

fn benchmark_parser(c: &mut Criterion) {
    c.bench_function("parse_bpv7_zero_copy", |b| {
        let dummy_payload = [0u8; 128];
        b.iter(|| {
            let _ = &dummy_payload[..];
        })
    });
}

criterion_group!(benches, benchmark_parser);
criterion_main!(benches);
