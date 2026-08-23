//! Fase 5: Profiling y auditoría de cero asignaciones
use core::sync::atomic::{AtomicU64, Ordering};

/// Lightweight, no_std telemetry counters and latency metrics using atomics.
pub struct Metrics {
	packets_processed: AtomicU64,
	packets_dropped: AtomicU64,
	latency_sum_ns: AtomicU64,
	latency_count: AtomicU64,
	latency_min_ns: AtomicU64,
	latency_max_ns: AtomicU64,
}

pub struct MetricsSnapshot {
	pub packets_processed: u64,
	pub packets_dropped: u64,
	pub latency_count: u64,
	pub latency_sum_ns: u64,
	pub latency_min_ns: u64,
	pub latency_max_ns: u64,
}

impl Metrics {
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

	/// Increment processed packet counter (SPSC or MPMC safe via atomics).
	pub fn incr_processed(&self, n: u64) {
		self.packets_processed.fetch_add(n, Ordering::Release);
	}

	/// Increment dropped packet counter.
	pub fn incr_dropped(&self, n: u64) {
		self.packets_dropped.fetch_add(n, Ordering::Release);
	}

	/// Record observed latency in nanoseconds. Caller is responsible for
	/// providing nanosecond values (e.g., from a hardware timer). This
	/// function does not depend on any OS clock and is no_std-friendly.
	pub fn record_latency_ns(&self, latency_ns: u64) {
		self.latency_sum_ns.fetch_add(latency_ns, Ordering::AcqRel);
		self.latency_count.fetch_add(1, Ordering::AcqRel);

		// update min
		let _ = self.latency_min_ns.try_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
			if latency_ns < cur { Some(latency_ns) } else { None }
		});

		// update max
		let _ = self.latency_max_ns.try_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
			if latency_ns > cur { Some(latency_ns) } else { None }
		});
	}

	/// Snapshot current counters atomically (loads use Acquire semantics).
	pub fn snapshot(&self) -> MetricsSnapshot {
		let packets_processed = self.packets_processed.load(Ordering::Acquire);
		let packets_dropped = self.packets_dropped.load(Ordering::Acquire);
		let latency_sum_ns = self.latency_sum_ns.load(Ordering::Acquire);
		let latency_count = self.latency_count.load(Ordering::Acquire);
		let latency_min_ns = self.latency_min_ns.load(Ordering::Acquire);
		let latency_max_ns = self.latency_max_ns.load(Ordering::Acquire);
		MetricsSnapshot {
			packets_processed,
			packets_dropped,
			latency_count,
			latency_sum_ns,
			latency_min_ns: if latency_min_ns == u64::MAX { 0 } else { latency_min_ns },
			latency_max_ns,
		}
	}

	/// Reset metrics to zero. Use with care in concurrent contexts.
	pub fn reset(&self) {
		self.packets_processed.store(0, Ordering::Release);
		self.packets_dropped.store(0, Ordering::Release);
		self.latency_sum_ns.store(0, Ordering::Release);
		self.latency_count.store(0, Ordering::Release);
		self.latency_min_ns.store(u64::MAX, Ordering::Release);
		self.latency_max_ns.store(0, Ordering::Release);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn metrics_basic_counts_and_latency() {
		let m = Metrics::new();
		m.incr_processed(5);
		m.incr_dropped(2);
		m.record_latency_ns(100);
		m.record_latency_ns(200);
		m.record_latency_ns(50);
		let s = m.snapshot();
		assert_eq!(s.packets_processed, 5);
		assert_eq!(s.packets_dropped, 2);
		assert_eq!(s.latency_count, 3);
		assert_eq!(s.latency_sum_ns, 350);
		assert_eq!(s.latency_min_ns, 50);
		assert_eq!(s.latency_max_ns, 200);
	}
}
