use std::net::UdpSocket;
use std::thread;
use std::time::Duration;

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
    println!("=== Starting NASA-style Chaos Fault Injector (High-Speed Mode) ===");
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let target_addr = "127.0.0.1:4556";

    let mut pkt_count = 0u64;

    loop {
        pkt_count += 1;
        let mut bundle = make_valid_primary();

        match pkt_count % 4 {
            1 => {
                // Paquete válido nominal
                let _ = socket.send_to(&bundle, target_addr);
            }
            2 => {
                // Corrupción de bit por radiación espacial (Bit-Flip)
                bundle[0] ^= 0xFF;
                let _ = socket.send_to(&bundle, target_addr);
            }
            3 => {
                // Trama truncada por pérdida parcial en ráfaga
                let _ = socket.send_to(&bundle[..8], target_addr);
            }
            0 => {
                // Caída total de enlace (Blackout): Se omite la transmisión
            }
            _ => unreachable!(),
        }

        if pkt_count % 100_000 == 0 {
            println!("[CHAOS] Inyectados {} paquetes con patrones de falla...", pkt_count);
            thread::sleep(Duration::from_millis(10)); // Breve respiración para evitar saturación absoluta
        }
    }
}