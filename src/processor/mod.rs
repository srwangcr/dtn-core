//! Bundle processing stages: validation, extension-block policy, fragmentation
//! and the end-to-end ingestion pipeline.

/// Fixed-size fragment reassembly slots.
pub mod fragmentation;
/// Zero-copy ingestion pipeline wiring all modules together.
pub mod pipeline;
/// RFC 9171 bundle processing state machine.
pub mod state_machine;
