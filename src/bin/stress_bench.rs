use std::net::UdpSocket;
use std::time::Instant;

fn make_valid_primary() -> Vec<u8> {
    vec![
        0xA6,
        0x01, 0x07,
        0x02, 0x03,
        0x04, 0x64, b'd', b'e', b's', b't',
        0x05, 0x63, b's', b'r', b'c',
        0x06, 0x82, 0x19, 0x04, 0xD2, 0x01,
        0x07, 0x19, 0x0E, 0x10,
    ]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let target_addr = "127.0.0.1:4556";
    let bundle = make_valid_primary();

    let total_packets = 1_000_000u64;
    println!("=== Disparando Ráfaga de {} Paquetes UDP ===", total_packets);

    let start = Instant::now();

    for _ in 0..total_packets {
        let _ = socket.send_to(&bundle, target_addr);
    }

    let elapsed = start.elapsed();
    let total_bytes = total_packets * bundle.len() as u64;
    let mb_sent = (total_bytes as f64) / (1024.0 * 1024.0);
    let pps = (total_packets as f64) / elapsed.as_secs_f64();

    println!("=== Ráfaga de Emisión Finalizada ===");
    println!("Tiempo de Emisión: {:.2?}", elapsed);
    println!("Total Enviado: {:.2} MB", mb_sent);
    println!("Tasa de Emisión: {:.2} KPkts/sec", pps / 1000.0);

    Ok(())
}
