//! Convergence-layer adapters: UDP datagrams and COBS-framed serial links.

/// Stop-and-Wait ARQ over COBS with a lightweight 3-byte link header.
pub mod reliable;
/// Zero-allocation COBS-framed UART/RS-485 convergence layer.
pub mod serial;
/// Datagram-oriented UDP convergence layer.
pub mod udp;
