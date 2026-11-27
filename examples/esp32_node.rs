//! ESP32 / low-SRAM node configuration example for `dtn-core`.
//!
//! This example runs on the host so it can be executed with
//! `cargo run --example esp32_node`, but every buffer and parameter below is
//! chosen to fit the constraints of an ESP32 (520 KB SRAM on the classic
//! ESP32, ~320 KB usable after WiFi/BT reserves) or an ESP32-C3 RISC-V core.
//!
//! # Deployment
//!
//! The library itself is strictly `#![no_std]` and never allocates. On ESP32
//! it can be used in two ways:
//!
//! 1. **Bare-metal / ESP-IDF `no_std` task**: add `dtn-core` as a dependency of
//!    an `esp-idf-sys` / `esp-hal` firmware crate, configure the target
//!    (`xtensa-esp32-espidf` or `riscv32imc-esp-espidf`), and call the pipeline
//!    from a high-priority task or UART ISR. Feed bytes with
//!    [`SerialFraming::push_byte`] from the UART ISR and enqueue decoded
//!    bundles with [`IngestionPipeline`].
//! 2. **Host-supervised gateway**: run the example binaries (`dtnd`,
//!    `dtn-cli`) on a Linux gateway and bridge the ESP32 UART to UDP using the
//!    COBS framing shown below.
//!
//! # Low-SRAM budget (class sizes)
//!
//! The const-generic capacities below bound every buffer at compile time:
//!
//! | Component | Configuration | SRAM |
//! |-----------|---------------|------|
//! | `SerialFraming` RX/TX | 256 / 256 B | 512 B |
//! | `OutboundBundleQueue` | 4 slots x 256 B | ~1 KB |
//! | `ReassemblySlot` | 128 B ADU, 16 ranges (default) | ~400 B |
//! | `DirectWal` | 1 page (4096 B) | 4096 B |
//! | `CgrIntervalTree` | 8 contacts | ~128 B |
//!
//! Total static footprint is a few KB, leaving the rest of SRAM for the
//! application, network stack and RTOS.

use dtn_core::cla::serial::SerialFraming;
use dtn_core::processor::fragmentation::ReassemblySlot;
use dtn_core::processor::pipeline::{IngestionPipeline, RouteDecision};
use dtn_core::routing::interval_tree::{CgrIntervalTree, ContactInterval};
use dtn_core::storage::outbound::OutboundBundleQueue;
use dtn_core::storage::wal::DirectWal;
use dtn_core::telemetry::metrics::SystemMetrics;

/// Minimal BPv7 bundle used in this example: primary block + payload + break.
const BUNDLE: &[u8] = &[
    0x9f, 0x88, 0x07, 0x00, 0x00, 0x82, 0x01, 0x00, 0x82, 0x01, 0x00, 0x82, 0x01, 0x00, 0x82,
    0x00, 0x01, 0x19, 0xea, 0x60, 0x85, 0x01, 0x01, 0x00, 0x00, 0x41, 0x58, 0xff,
];

fn main() {
    // ---- COBS UART framing -------------------------------------------------
    let mut uart = SerialFraming::<256, 256>::new();
    let mut wire = [0u8; 256];
    let wire_len = {
        let frame = uart.encode_frame(BUNDLE).unwrap();
        let len = frame.len();
        wire[..len].copy_from_slice(frame);
        len
    };
    println!("COBS frame: {wire_len} bytes (bundle: {} bytes)", BUNDLE.len());

    // Feed the frame back one byte at a time, as a UART ISR would.
    let mut decoded = [0u8; 256];
    let decoded_len = {
        let mut len = 0usize;
        for &byte in &wire[..wire_len] {
            if let Some(frame) = uart.push_byte(byte).unwrap() {
                len = frame.len();
                decoded[..len].copy_from_slice(frame);
            }
        }
        len
    };
    assert_eq!(decoded_len, BUNDLE.len(), "COBS round-trip length mismatch");
    let bundle = &decoded[..decoded_len];

    // ---- Contact plan ------------------------------------------------------
    let mut router = CgrIntervalTree::<8>::new();
    // A contact to node 42 that is active at the simulated current time.
    let _ = router.insert(ContactInterval::new(0, 10_000, 42));

    // ---- Static outbound queue, WAL and metrics -----------------------------
    let mut outbound = OutboundBundleQueue::<4, 256>::new();
    let mut wal = DirectWal::<1>::new();
    let mut slot = ReassemblySlot::<128>::new();
    let metrics = SystemMetrics::new();

    // ---- Ingestion ---------------------------------------------------------
    let decision = IngestionPipeline::ingest_and_route::<8, 4, 256, 128, 1>(
        bundle,
        1_000,
        &router,
        &mut outbound,
        Some(&mut slot),
        Some(&mut wal),
        Some(&metrics),
    );
    match decision {
        RouteDecision::ForwardNextHop { node_id, start_time } => {
            if let Some((queued_hop, queued_bundle)) = outbound.front_parts() {
                println!(
                    "Forwarding to node {node_id} at t={start_time}: queued hop={queued_hop}, {} bundle bytes",
                    queued_bundle.len()
                );
                outbound.pop();
            }
        }
        other => println!("Pipeline decision: {other:?}"),
    }

    let snapshot = metrics.snapshot();
    println!(
        "Metrics: processed={} dropped={} avg_latency_ns={}",
        snapshot.packets_processed, snapshot.packets_dropped, snapshot.latency_avg_ns
    );
}
