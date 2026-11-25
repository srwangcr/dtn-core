//! Allocation-free storage: SPSC ring buffer, CRC32C, WAL and disk sink.

/// Fixed-size block device simulation with crash-recovery support.
pub mod disk_sink;
/// Table-free CRC32C (Castagnoli) implementation.
pub mod crc32c;
/// Lock-free SPSC byte ring buffer.
pub mod ring_buffer;
/// Page-structured write-ahead log and recovery scanner.
pub mod wal;
