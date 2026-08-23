use dtn_core::cla::udp::UdpFrameBuffer;
use dtn_core::routing::interval_tree::{CgrIntervalTree, ContactInterval};
use dtn_core::storage::ring_buffer::LockFreeRingBuffer;
use dtn_core::storage::wal::DirectWal;
use dtn_core::telemetry::metrics::SystemMetrics;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn get_dtn_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Starting dtn-core Engine Daemon (BPv7) ===");

    // Control para apagado controlado (Graceful Shutdown)
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    if let Err(e) = ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    }) {
        eprintln!("[WARN] Could not set CTRL+C handler: {}", e);
    }

    // Bind del socket UDP nativo en puerto 4556 (Puerto estándar IANA para DTN)
    let socket = UdpSocket::bind("0.0.0.0:4556")?;
    socket.set_nonblocking(true)?;
    println!("[CLA-UDP] Listening on 0.0.0.0:4556 (Non-blocking)");

    // Estado interno del motor
    let mut router = CgrIntervalTree::<64>::new();
    let _ = router.insert(ContactInterval {
        start_time: 0,
        end_time: 10_000_000_000,
        target_node_id: 200,
    });

    let mut ring = LockFreeRingBuffer::<4096>::new();
    let (mut producer, mut consumer) = ring.split();
    let mut wal = DirectWal::<8>::new();
    let metrics = SystemMetrics::new();
    let mut cla_buf = UdpFrameBuffer::<1500>::new();

    let mut rx_bytes = [0u8; 1500];
    let mut drained_buf = [0u8; 8];

    println!("[CORE] Engine initialized. Waiting for packets...");

    while running.load(Ordering::Relaxed) {
        match socket.recv_from(&mut rx_bytes) {
            Ok((len, src)) => {
                let raw_data = &rx_bytes[..len];

                // 2. Ingesta en el buffer CLA
                if cla_buf.ingest_raw_packet(raw_data).is_ok() {
                    // Timestamp UNIX POSIX real en segundos para validación BPv7
                    let current_ts = get_dtn_unix_timestamp();

                    let decision = cla_buf.process_to_pipeline::<64, 4096, 1024, 8>(
                        current_ts,
                        &router,
                        &mut producer,
                        Some(&mut wal),
                        Some(&metrics),
                    );

                    println!("[RECV] From: {} | Size: {}B | Decision: {:?}", src, len, decision);

                    // Consumir el ring buffer si hay elementos enrutados
                    if consumer.pop(&mut drained_buf) > 0 {
                        let target = u64::from_le_bytes(drained_buf);
                        println!("[FORWARD] Enqueued target node ID: {}", target);
                    }
                }
                cla_buf.clear();
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Yield de CPU cuando no hay tráfico de red entrante
                std::thread::sleep(Duration::from_micros(100));
            }
            Err(e) => {
                eprintln!("[ERROR] Socket error: {}", e);
            }
        }
    }

    println!("[CORE] Shutdown signal received. Cleaning up resources...");
    Ok(())
}