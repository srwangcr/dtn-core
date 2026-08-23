use std::env;
use std::net::UdpSocket;

fn print_usage() {
    println!("dtn-cli v0.1.0 - DTN Engine Control Tool");
    println!("Usage:");
    println!("  dtn-cli send <target_ip:port> <message>");
    println!("  dtn-cli ping <target_ip:port>");
    println!("Examples:");
    println!("  dtn-cli send 127.0.0.1:4556 \"Hello Outer Space\"");
    println!("  dtn-cli ping 127.0.0.1:4556");
}

fn make_valid_primary_with_payload(payload: &[u8]) -> Vec<u8> {
    let mut bundle = vec![
        0xA6,
        0x01, 0x07,
        0x02, 0x03,
        0x04, 0x64, b'd', b'e', b's', b't',
        0x05, 0x63, b's', b'r', b'c',
        0x06, 0x82, 0x19, 0x04, 0xD2, 0x01,
        0x07, 0x19, 0x0E, 0x10,
    ];
    bundle.extend_from_slice(payload);
    bundle
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 {
        print_usage();
        return Ok(());
    }

    let command = &args[1];
    let target_addr = &args[2];
    let socket = UdpSocket::bind("0.0.0.0:0")?;

    match command.as_str() {
        "send" => {
            if args.len() < 4 {
                println!("Error: Missing message payload.");
                return Ok(());
            }
            let message = &args[3];
            let raw_bundle = make_valid_primary_with_payload(message.as_bytes());
            socket.send_to(&raw_bundle, target_addr)?;
            println!("[CLI] Valid BPv7 bundle sent to {} ({} bytes)", target_addr, raw_bundle.len());
        }
        "ping" => {
            let ping_bundle = make_valid_primary_with_payload(b"PING");
            socket.send_to(&ping_bundle, target_addr)?;
            println!("[CLI] Ping bundle dispatched to {}", target_addr);
        }
        _ => {
            print_usage();
        }
    }

    Ok(())
}