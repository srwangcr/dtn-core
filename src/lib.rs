#![no_std]
#![deny(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]
//! # `dtn-core`
//!
//! A deterministic, zero-allocation Bundle Protocol version 7 (RFC 9171)
//! engine for deeply embedded systems.
//!
//! Design invariants:
//!
//! * **No heap.** The crate is strictly `#![no_std]`; it neither links `alloc`
//!   nor performs dynamic allocation. Every buffer is a caller-owned slice or
//!   a `const`-sized array, so memory footprints are known at compile time.
//! * **Zero copy with explicit lifetimes.** Parsers borrow from the original
//!   packet slice; no BP header or payload is duplicated.
//! * **Explicit errors.** Fallible operations return [`error::DtnResult`] with
//!   a `Copy` [`error::DtnError`] discriminant, never a panic on hostile input.
//! * **Weak-memory correctness.** SPSC ring-buffer hand-offs use acquire /
//!   release atomics; counters use relaxed atomics where no data is published.
//!
//! Example (host or firmware with a monotonic clock):
//!
//! ```
//! use dtn_core::processor::state_machine::{BundleProcessor, ProcessStatus};
//!
//! // Minimal RFC 9171 bundle: indefinite array, primary block, payload, break.
//! let bundle = [
//!     0x9f,                                   // indefinite-length bundle array
//!     0x88, 0x07, 0x00, 0x00,                 // primary block: 8 fields
//!     0x82, 0x01, 0x00,                       // destination EID: dtn:none
//!     0x82, 0x01, 0x00,                       // source node ID: dtn:none
//!     0x82, 0x01, 0x00,                       // report-to EID: dtn:none
//!     0x82, 0x00, 0x01,                       // creation timestamp [0, 1]
//!     0x19, 0xea, 0x60,                       // lifetime 60000 ms
//!     0x85, 0x01, 0x01, 0x00, 0x00,           // payload block header
//!     0x45, b'h', b'e', b'l', b'l', b'o',     // payload "hello"
//!     0xff,                                   // end of bundle array
//! ];
//! let result = BundleProcessor::process_bundle(&bundle, 1_000, None);
//! assert_eq!(result.status, ProcessStatus::Accepted);
//! ```
//!
//! The primary block above contains the eight mandatory fields of a
//! non-fragmented bundle without CRC: version, processing flags, CRC type,
//! destination EID, source node ID, report-to EID, creation timestamp and
//! lifetime (milliseconds). A clock-less node sets creation time to zero and
//! uses a Bundle Age Block instead (block type 7).

/// BPSec Block Integrity Blocks (RFC 9172 ASB / RFC 9173 BIB-HMAC-SHA2).
pub mod bpsec;
/// Explicit error codes used throughout the crate.
pub mod error;
/// Convergence-layer adapters (UDP and COBS-framed serial).
pub mod cla;
/// Zero-copy RFC 9171 CBOR decoders.
pub mod parser;
/// Bundle processing, state machine, fragmentation and ingestion pipeline.
pub mod processor;
/// Static Contact Plan Scheduler / Interval Selector (single-hop EAT lookup; full multi-hop CGR is future work).
pub mod routing;
/// Lock-free queue, write-ahead log and disk sink with CRC32C recovery.
pub mod storage;
/// Lock-free, `no_std` telemetry counters.
pub mod telemetry;
