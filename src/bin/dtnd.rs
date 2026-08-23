use dtn_core::cla::udp::UdpFrameBuffer;
use dtn_core::routing::interval_tree::{CgrIntervalTree, ContactInterval};
use dtn_core::storage::ring_buffer::LockFreeRingBuffer;
use dtn_core::storage::wal::DirectWal;
use dtn_core::telemetry::metrics::SystemMetrics;
use std::net::UdpSocket;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Starting dtn-core Engine Daemon (BPv7) ===");

    // Bind del socket UDP nativo en puerto 4556 (Puerto estándar IANA para DTN)
    let socket = UdpSocket::bind("0.0.0.0:4556")?;
    socket.set_nonblocking(true)?;
    println!("[CLA-UDP] Listening on 0.0.0.0:4556 (Non-blocking)");

    // Estado interno del motor
    let mut router = CgrIntervalTree::<64>::new();
    // Contacto inicial por defecto
    let _ = router.insert(ContactInterval {
        start_time: 0,
        end_time: 10_000_000,
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

    loop {
        // 1. Recibir datos del socket del sistema operativo
        match socket.recv_from(&mut rx_bytes) {
            Ok((len, src)) => {
                let raw_data = &rx_bytes[..len];

                // 2. Ingesta en el buffer CLA
                if cla_buf.ingest_raw_packet(raw_data).is_ok() {
                    // 3. Ingesta y enrutamiento en Hot-Path
                    let current_ts = 1000; // Timestamp simulado o de sistema
                    let decision = cla_buf.process_to_pipeline::<64, 4096, 1024, 8>(
                        current_ts,
                        &router,
                        &mut producer,
                        Some(&mut wal),
                        Some(&metrics),
                    );

                    println!("[RECV] From: {} | Size: {}B | Decision: {:?}", src, len, decision);

                    // Consumir el ring buffer si hay elementos
                    if consumer.pop(&mut drained_buf) > 0 {
                        let target = u64::from_le_bytes(drained_buf);
                        println!("[FORWARD] Enqueued target node ID: {}", target);
                    }
                }
                cla_buf.clear();
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Sin paquetes entrantes: yield de CPU para no quemar el core al 100% en idle
                std::thread::sleep(std::time::Duration::from_micros(100));
            }
            Err(e) => {
                eprintln!("[ERROR] Socket error: {}", e);
            }
        }
    }
}
