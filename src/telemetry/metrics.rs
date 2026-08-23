//! Módulo de métricas para telemetría interna y monitoreo de rendimiento.

use core::sync::atomic::{AtomicU64, Ordering};

pub struct SystemMetrics {
    pub packets_processed: AtomicU64,
    pub packets_dropped: AtomicU64,
    pub latency_sum_ns: AtomicU64,
    pub latency_count: AtomicU64,
    pub latency_min_ns: AtomicU64,
    pub latency_max_ns: AtomicU64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub packets_processed: u64,
    pub packets_dropped: u64,
    pub latency_avg_ns: u64,
    pub latency_min_ns: u64,
    pub latency_max_ns: u64,
}

impl SystemMetrics {
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

    pub fn record_packet(&self) {
        self.packets_processed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_drop(&self) {
        self.packets_dropped.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_latency(&self, val: u64) {
        self.latency_sum_ns.fetch_add(val, Ordering::Relaxed);
        self.latency_count.fetch_add(1, Ordering::Relaxed);

        #[allow(deprecated)]
        let _ = self.latency_min_ns.fetch_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
            if val < cur { Some(val) } else { None }
        });

        #[allow(deprecated)]
        let _ = self.latency_max_ns.fetch_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
            if val > cur { Some(val) } else { None }
        });
    }

    /// Captura una foto de las métricas.
    /// NOTA: Las lecturas individuales no son atómicas entre sí; el snapshot representa
    /// una coherencia eventual pensada para telemetría continua.
    pub fn snapshot(&self) -> MetricsSnapshot {
        let processed = self.packets_processed.load(Ordering::Relaxed);
        let dropped = self.packets_dropped.load(Ordering::Relaxed);
        let sum = self.latency_sum_ns.load(Ordering::Relaxed);
        let count = self.latency_count.load(Ordering::Relaxed);
        let min_val = self.latency_min_ns.load(Ordering::Relaxed);
        let max_val = self.latency_max_ns.load(Ordering::Relaxed);

        let avg = sum.checked_div(count).unwrap_or(0);
        let min = if min_val == u64::MAX { 0 } else { min_val };

        MetricsSnapshot {
            packets_processed: processed,
            packets_dropped: dropped,
            latency_avg_ns: avg,
            latency_min_ns: min,
            latency_max_ns: max_val,
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
    fn metrics_basic_counts_and_latency() {
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
}