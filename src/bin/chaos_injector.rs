use std::env;
use std::net::UdpSocket;
use std::process;
use std::thread;
use std::time::Duration;

fn valid_bundle() -> Vec<u8> {
    vec![
        0xA6,
        0x01, 0x07,
        0x02, 0x03,
        0x04, 0x64, b'd', b'e', b's', b't',
        0x05, 0x63, b's', b'r', b'c',
        0x06, 0x82, 0x19, 0x04, 0xD2, 0x01,
        0x07, 0x19, 0x0E, 0x10,
        0x85, 0x01, 0x01, 0x00, 0x00, 0x45,
        b'h', b'e', b'l', b'l', b'o',
    ]
}

fn expired_bundle() -> Vec<u8> {
    let mut bundle = valid_bundle();
    // Shorten the lifetime so it becomes expired immediately.
    // Same overall format, different semantic status.
    if bundle.len() >= 18 {
        bundle[18] = 0x00;
        bundle[19] = 0x00;
    }
    bundle
}

fn missing_payload_bundle() -> Vec<u8> {
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

fn truncated_payload_bundle() -> Vec<u8> {
    let mut bundle = valid_bundle();
    bundle.truncate(bundle.len() - 3);
    bundle
}

fn bitflip_bundle() -> Vec<u8> {
    let mut bundle = valid_bundle();
    if !bundle.is_empty() {
        bundle[0] ^= 0xFF;
    }
    bundle
}

fn noise_bundle(seed: u64) -> Vec<u8> {
    let mut out = vec![0u8; 32];
    let mut x = seed ^ 0xA5A5_5A5A_5A5A_5A5A;

    for byte in &mut out {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *byte = ((x >> 8) & 0xFF) as u8;
    }

    out
}

fn usage() -> &'static str {
    "usage: chaos_injector [--target 127.0.0.1:4556] [--interval-ms 150] [--count 0] [--once]

Examples:
  chaos_injector
  chaos_injector --target 127.0.0.1:4556 --interval-ms 100 --count 1000
  chaos_injector --once --target 127.0.0.1:4556"
}

fn parse_u64_arg(args: &[String], flag: &str, default: u64) -> u64 {
    for i in 0..args.len() {
        if args[i] == flag {
            if i + 1 < args.len() {
                return args[i + 1].parse().unwrap_or(default);
            }
            break;
        }
    }
    default
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{}", usage());
        return Ok(());
    }

    let target = args
        .iter()
        .zip(args.iter().skip(1))
        .find_map(|(a, b)| (a == "--target").then_some(b.as_str()))
        .unwrap_or("127.0.0.1:4556")
        .to_string();

    let interval_ms = parse_u64_arg(&args, "--interval-ms", 150);
    let count_limit = parse_u64_arg(&args, "--count", 0);
    let once_only = args.iter().any(|arg| arg == "--once");

    let socket = UdpSocket::bind("0.0.0.0:0")?;
    println!("=== Chaos injector active ===");
    println!("target: {target}");
    println!("interval: {interval_ms} ms");
    println!("count limit: {} (0 = infinite)", count_limit);
    println!("patterns: valid, expired, missing-payload, truncated, bitflip, noise");

    let mut sent = 0u64;
    let mut seed = 0xC0FFEEu64;

    loop {
        let payload = match sent % 6 {
            0 => valid_bundle(),
            1 => expired_bundle(),
            2 => missing_payload_bundle(),
            3 => truncated_payload_bundle(),
            4 => bitflip_bundle(),
            _ => noise_bundle(seed),
        };

        if let Err(err) = socket.send_to(&payload, &target) {
            eprintln!("send failed: {err}");
            process::exit(1);
        }

        println!(
            "sent #{sent}: pattern={} len={}",
            match sent % 6 {
                0 => "valid",
                1 => "expired",
                2 => "missing-payload",
                3 => "truncated",
                4 => "bitflip",
                _ => "noise",
            },
            payload.len()
        );

        sent += 1;
        seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);

        if count_limit > 0 && sent >= count_limit {
            break;
        }

        if once_only {
            break;
        }

        thread::sleep(Duration::from_millis(interval_ms as u64));
    }

    Ok(())
}