use std::time::{Duration, Instant};

use dtn_core::processor::state_machine::{BundleProcessor, ProcessStatus};
use dtn_core::telemetry::metrics::SystemMetrics;

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
    vec![
        0xA6,
        0x01, 0x07,
        0x02, 0x03,
        0x04, 0x64, b'd', b'e', b's', b't',
        0x05, 0x63, b's', b'r', b'c',
        0x06, 0x82, 0x19, 0x04, 0xD2, 0x01,
        0x07, 0x19, 0x00, 0x01,
        0x85, 0x01, 0x01, 0x00, 0x00, 0x45,
        b'h', b'e', b'l', b'l', b'o',
    ]
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

fn truncated_bundle() -> Vec<u8> {
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

#[test]
fn chaos_matrix_valid_expired_malformed_and_noise() {
    let cases = [
        ("valid", valid_bundle(), ProcessStatus::Accepted),
        ("expired", expired_bundle(), ProcessStatus::Expired),
        ("missing_payload", missing_payload_bundle(), ProcessStatus::RejectedMalformed),
        ("truncated", truncated_bundle(), ProcessStatus::RejectedMalformed),
        ("bitflip", bitflip_bundle(), ProcessStatus::RejectedMalformed),
        ("noise", noise_bundle(0xC0FFEE), ProcessStatus::RejectedMalformed),
    ];

    for (name, bytes, expected) in cases {
        let metrics = SystemMetrics::new();
        let result = BundleProcessor::process_bundle(&bytes, 2000, Some(&metrics));
        assert_eq!(result.status, expected, "case failed: {name}");
    }
}

#[test]
fn chaos_matrix_stress_small_batches() {
    let mut accepted = 0usize;
    let mut expired = 0usize;
    let mut malformed = 0usize;

    let start = Instant::now();
    for i in 0..2_000 {
        let payload = match i % 6 {
            0 => valid_bundle(),
            1 => expired_bundle(),
            2 => missing_payload_bundle(),
            3 => truncated_bundle(),
            4 => bitflip_bundle(),
            _ => noise_bundle(i as u64),
        };

        let metrics = SystemMetrics::new();
        let result = BundleProcessor::process_bundle(&payload, 2000, Some(&metrics));

        match result.status {
            ProcessStatus::Accepted => accepted += 1,
            ProcessStatus::Expired => expired += 1,
            ProcessStatus::RejectedMalformed => malformed += 1,
        }
    }

    let elapsed = start.elapsed();
    assert!(accepted > 0, "expected some valid bundles in the stress matrix");
    assert!(expired > 0, "expected some expired bundles in the stress matrix");
    assert!(malformed > 0, "expected malformed bundles in the stress matrix");
    assert!(elapsed < Duration::from_secs(3), "stress matrix took too long: {elapsed:?}");
}
