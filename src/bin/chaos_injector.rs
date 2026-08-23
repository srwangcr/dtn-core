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
    println!("=== Starting NASA-style Chaos Fault Injector ===");
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let target_addr = "127.0.0.1:4556";

    let mut pkt_count = 0;

    loop {
        pkt_count += 1;
        let mut bundle = make_valid_primary();

        // Modo de Inyección de Fallas
        match pkt_count % 4 {
            1 => {
                println!("[CHAOS] Scenario 1: Nominal Valid Bundle #{}", pkt_count);
                socket.send_to(&bundle, target_addr)?;
            }
            2 => {
                println!("[CHAOS] Scenario 2: Bit-Flip Radiation Damage on Bundle #{}", pkt_count);
                bundle[0] ^= 0xFF; // Corromper byte inicial
                socket.send_to(&bundle, target_addr)?;
            }
            3 => {
                println!("[CHAOS] Scenario 3: Truncated Payload / Burst Loss on Bundle #{}", pkt_count);
                socket.send_to(&bundle[..8], target_addr)?; // Truncar paquete
            }
            0 => {
                println!("[CHAOS] Scenario 4: Blackout / Total Contact Drop (Skipping Packet #{})", pkt_count);
                // No se envía nada para emular pérdida de enlace
            }
            _ => unreachable!(),
        }

        thread::sleep(Duration::from_millis(500));
    }
}
