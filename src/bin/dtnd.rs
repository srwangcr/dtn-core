use dtn_core::cla::udp::UdpFrameBuffer;
use dtn_core::routing::interval_tree::{CgrIntervalTree, ContactInterval};
use dtn_core::storage::ring_buffer::LockFreeRingBuffer;
use dtn_core::storage::wal::DirectWal;
use dtn_core::telemetry::metrics::SystemMetrics;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static PACKETS_PROCESSED: AtomicU64 = AtomicU64::new(0);
static BYTES_PROCESSED: AtomicU64 = AtomicU64::new(0);

fn get_dtn_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Starting dtn-core Engine Daemon (BPv7) ===");

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    if let Err(e) = ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    }) {
        eprintln!("[WARN] Could not set CTRL+C handler: {}", e);
    }

    let socket = UdpSocket::bind("0.0.0.0:4556")?;
    
    // Timeout de recepción para que el socket no bloquee infinitamente al apagar
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;

    println!("[CLA-UDP] Listening on 0.0.0.0:4556 (High-Throughput Sync Mode)");

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

    // Hilo de Telemetría cada 1 segundo
    let running_telemetry = running.clone();
    let telemetry_handle = thread::spawn(move || {
        let mut last_bytes = 0u64;
        let mut last_pkts = 0u64;

        while running_telemetry.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_secs(1));
            let curr_bytes = BYTES_PROCESSED.load(Ordering::Relaxed);
            let curr_pkts = PACKETS_PROCESSED.load(Ordering::Relaxed);

            let delta_bytes = curr_bytes - last_bytes;
            let delta_pkts = curr_pkts - last_pkts;

            if delta_pkts > 0 {
                let mb_s = (delta_bytes as f64) / (1024.0 * 1024.0);
                println!(
                    "[METRICS] Ingesta Red: {:.2} MB/s | Throughput: {} pkts/s | Total Ingestados: {}",
                    mb_s, delta_pkts, curr_pkts
                );
            }

            last_bytes = curr_bytes;
            last_pkts = curr_pkts;
        }
    });

    println!("[CORE] Engine initialized. Listening for high-speed burst...");

    while running.load(Ordering::Relaxed) {
        if let Ok((len, _src)) = socket.recv_from(&mut rx_bytes) {
            let raw_data = &rx_bytes[..len];

            if cla_buf.ingest_raw_packet(raw_data).is_ok() {
                let current_ts = get_dtn_unix_timestamp();

                let _decision = cla_buf.process_to_pipeline::<64, 4096, 1024, 8>(
                    current_ts,
                    &router,
                    &mut producer,
                    Some(&mut wal),
                    Some(&metrics),
                );

                if consumer.pop(&mut drained_buf) > 0 {
                    // Paquete procesado, enrutado y contabilizado
                }
            }
            
            PACKETS_PROCESSED.fetch_add(1, Ordering::Relaxed);
            BYTES_PROCESSED.fetch_add(len as u64, Ordering::Relaxed);
            cla_buf.clear();
        }
    }

    println!("[CORE] Shutdown signal received. Cleaning up resources...");
    telemetry_handle.join().ok();
    Ok(())
}