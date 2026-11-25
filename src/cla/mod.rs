//! Convergence-layer adapters: UDP datagrams and COBS-framed serial links.

/// Zero-allocation COBS-framed UART/RS-485 convergence layer.
pub mod serial;
/// Datagram-oriented UDP convergence layer.
pub mod udp;
