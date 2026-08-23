use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use dtn_core::storage::ring_buffer::LockFreeRingBuffer;

fn main() {
    println!("=== DTN-Core: SPSC Lock-Free Benchmarking ===");

    const BUFFER_SIZE: usize = 65536; // 64 KB Ring Buffer
    const TOTAL_OPERATIONS: usize = 10_000_000;

    let mut rb: LockFreeRingBuffer<BUFFER_SIZE> = LockFreeRingBuffer::new();
    let (mut producer, mut consumer) = rb.split();

    let done = Arc::new(AtomicBool::new(false));
    let done_clone = done.clone();

    let payload = [0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04]; // 8 bytes packet

    let start = Instant::now();

    // Hilo Consumidor
    let consumer_handle = thread::spawn(move || {
        let mut read_bytes = 0usize;
        let mut buf = [0u8; 8];
        
        while !done_clone.load(Ordering::Acquire) || read_bytes < TOTAL_OPERATIONS * payload.len() {
            let popped = consumer.pop(&mut buf);
            read_bytes += popped;
            if popped == 0 {
                thread::yield_now();
            }
        }
        read_bytes
    });

    // Hilo Productor
    let producer_handle = thread::spawn(move || {
        let mut pushed_bytes = 0usize;
        for _ in 0..TOTAL_OPERATIONS {
            while producer.push(&payload).is_err() {
                thread::yield_now();
            }
            pushed_bytes += payload.len();
        }
        done.store(true, Ordering::Release);
        pushed_bytes
    });

    let prod_bytes = producer_handle.join().unwrap();
    let cons_bytes = consumer_handle.join().unwrap();
    let elapsed = start.elapsed();

    let total_mb = (prod_bytes as f64) / (1024.0 * 1024.0);
    let ops_per_sec = (TOTAL_OPERATIONS as f64) / elapsed.as_secs_f64();
    let throughput_mbps = total_mb / elapsed.as_secs_f64();

    println!("Tiempo total    : {:.3?}", elapsed);
    println!("Bytes procesados : {} bytes ({:.2} MB)", prod_bytes, total_mb);
    println!("Throughput Ops  : {:.2} ops/sec", ops_per_sec);
    println!("Bandwidth       : {:.2} MB/s", throughput_mbps);

    assert_eq!(prod_bytes, cons_bytes, "Discrepancia entre bytes producidos y consumidos");
}