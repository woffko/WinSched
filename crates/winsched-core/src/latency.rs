//! Bounded scheduler wake-latency telemetry.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// Rolling latency percentiles published to the UI and event log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulerLatencyStatus {
    pub enabled: bool,
    pub total_samples: u64,
    pub window_samples: usize,
    pub last_lateness_us: u64,
    pub p50_lateness_us: u64,
    pub p95_lateness_us: u64,
    pub p99_lateness_us: u64,
    pub maximum_lateness_us: u64,
}

/// Fixed-capacity sample window with deterministic nearest-rank percentiles.
#[derive(Debug, Clone)]
pub struct SchedulerLatencyWindow {
    capacity: usize,
    total_samples: u64,
    samples: VecDeque<u64>,
}

impl SchedulerLatencyWindow {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            total_samples: 0,
            samples: VecDeque::with_capacity(capacity.max(1)),
        }
    }

    pub fn record(&mut self, lateness_us: u64) {
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(lateness_us);
        self.total_samples = self.total_samples.saturating_add(1);
    }

    pub fn clear(&mut self) {
        self.total_samples = 0;
        self.samples.clear();
    }

    #[must_use]
    pub fn status(&self, enabled: bool) -> SchedulerLatencyStatus {
        let mut sorted = self.samples.iter().copied().collect::<Vec<_>>();
        sorted.sort_unstable();
        SchedulerLatencyStatus {
            enabled,
            total_samples: self.total_samples,
            window_samples: sorted.len(),
            last_lateness_us: self.samples.back().copied().unwrap_or(0),
            p50_lateness_us: nearest_rank(&sorted, 50),
            p95_lateness_us: nearest_rank(&sorted, 95),
            p99_lateness_us: nearest_rank(&sorted, 99),
            maximum_lateness_us: sorted.last().copied().unwrap_or(0),
        }
    }
}

fn nearest_rank(sorted: &[u64], percentile: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = sorted.len().saturating_mul(percentile).div_ceil(100);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_deterministic_nearest_rank_percentiles() {
        let mut window = SchedulerLatencyWindow::new(100);
        for sample in 1..=100 {
            window.record(sample);
        }
        let status = window.status(true);
        assert_eq!(status.total_samples, 100);
        assert_eq!(status.window_samples, 100);
        assert_eq!(status.last_lateness_us, 100);
        assert_eq!(status.p50_lateness_us, 50);
        assert_eq!(status.p95_lateness_us, 95);
        assert_eq!(status.p99_lateness_us, 99);
        assert_eq!(status.maximum_lateness_us, 100);
    }

    #[test]
    fn discards_old_samples_at_fixed_capacity() {
        let mut window = SchedulerLatencyWindow::new(3);
        for sample in [100, 200, 300, 400] {
            window.record(sample);
        }
        let status = window.status(true);
        assert_eq!(status.total_samples, 4);
        assert_eq!(status.window_samples, 3);
        assert_eq!(status.p50_lateness_us, 300);
        assert_eq!(status.maximum_lateness_us, 400);

        window.clear();
        assert_eq!(window.status(false), SchedulerLatencyStatus::default());
    }
}
