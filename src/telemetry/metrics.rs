//! Lock-free telemetry counters suitable for `no_std` and Miri.
//!
//! All counters are independent: `Relaxed` atomics are sufficient because no
//! other memory is published through them. Min/max updates use the atomic
//! `fetch_min`/`fetch_max` read-modify-write operations so they are correct on
//! weakly ordered cores without explicit CAS loops or deprecated orderings.
//! `portable_atomic` transparently falls back to a lock-based implementation
//! on targets without native 64-bit atomics (e.g. some RISC-V cores).

use portable_atomic::{AtomicU64, Ordering};

/// Lock-free counters for packets, drops and latency statistics.
pub struct SystemMetrics {
    /// Number of bundles submitted to the state machine.
    pub packets_processed: AtomicU64,
    /// Number of bundles rejected or dropped.
    pub packets_dropped: AtomicU64,
    /// Saturating accumulator used to compute the average.
    pub latency_sum_ns: AtomicU64,
    /// Number of latency samples.
    pub latency_count: AtomicU64,
    /// Minimum observed latency (`u64::MAX` before the first sample).
    pub latency_min_ns: AtomicU64,
    /// Maximum observed latency.
    pub latency_max_ns: AtomicU64,
}

/// Consistent-at-snapshot-time view of [`SystemMetrics`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricsSnapshot {
    /// Bundles processed at the time of the snapshot.
    pub packets_processed: u64,
    /// Bundles dropped at the time of the snapshot.
    pub packets_dropped: u64,
    /// Average latency in nanoseconds (0 when no samples exist).
    pub latency_avg_ns: u64,
    /// Minimum latency in nanoseconds (0 when no samples exist).
    pub latency_min_ns: u64,
    /// Maximum latency in nanoseconds.
    pub latency_max_ns: u64,
}

impl SystemMetrics {
    /// Creates a zeroed metrics object in `const` context.
    pub const fn new() -> Self {
        Self {
            packets_processed: AtomicU64::new(0),
            packets_dropped: AtomicU64::new(0),
            latency_sum_ns: AtomicU64::new(0),
            latency_count: AtomicU64::new(0),
            latency_min_ns: AtomicU64::new(u64::MAX),
            latency_max_ns: AtomicU64::new(0),
        }
    }

    /// Records one processed bundle.
    pub fn record_packet(&self) {
        self.packets_processed.fetch_add(1, Ordering::Relaxed);
    }

    /// Records one dropped bundle.
    pub fn record_drop(&self) {
        self.packets_dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a latency sample in nanoseconds.
    ///
    /// The sum saturates instead of wrapping, so a hostile or corrupted
    /// timestamp cannot make the reported average decrease.
    pub fn record_latency(&self, value: u64) {
        let _ = self.latency_sum_ns.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
            cur.checked_add(value)
        });
        self.latency_count.fetch_add(1, Ordering::Relaxed);
        self.latency_min_ns.fetch_min(value, Ordering::Relaxed);
        self.latency_max_ns.fetch_max(value, Ordering::Relaxed);
    }

    /// Captures an eventually consistent snapshot.
    ///
    /// Individual loads are atomic, but the four counters are not sampled as
    /// one atomic transaction; this is intended for continuous telemetry.
    pub fn snapshot(&self) -> MetricsSnapshot {
        let processed = self.packets_processed.load(Ordering::Relaxed);
        let dropped = self.packets_dropped.load(Ordering::Relaxed);
        let sum = self.latency_sum_ns.load(Ordering::Relaxed);
        let count = self.latency_count.load(Ordering::Relaxed);
        let min_value = self.latency_min_ns.load(Ordering::Relaxed);
        let max_value = self.latency_max_ns.load(Ordering::Relaxed);
        MetricsSnapshot {
            packets_processed: processed,
            packets_dropped: dropped,
            latency_avg_ns: sum.checked_div(count).unwrap_or(0),
            latency_min_ns: if min_value == u64::MAX { 0 } else { min_value },
            latency_max_ns: max_value,
        }
    }
}

impl Default for SystemMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_latency() {
        let metrics = SystemMetrics::new();
        metrics.record_packet();
        metrics.record_packet();
        metrics.record_drop();
        metrics.record_latency(100);
        metrics.record_latency(200);
        let snap = metrics.snapshot();
        assert_eq!(snap.packets_processed, 2);
        assert_eq!(snap.packets_dropped, 1);
        assert_eq!(snap.latency_avg_ns, 150);
        assert_eq!(snap.latency_min_ns, 100);
        assert_eq!(snap.latency_max_ns, 200);
    }

    #[test]
    fn empty_snapshot_is_zeroed() {
        let snap = SystemMetrics::new().snapshot();
        assert_eq!(snap.latency_avg_ns, 0);
        assert_eq!(snap.latency_min_ns, 0);
        assert_eq!(snap.latency_max_ns, 0);
    }

    #[test]
    fn latency_sum_saturates() {
        let metrics = SystemMetrics::new();
        metrics.record_latency(u64::MAX);
        metrics.record_latency(1);
        let snap = metrics.snapshot();
        assert_eq!(snap.latency_avg_ns, u64::MAX / 2);
    }
}
