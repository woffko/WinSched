//! Fixed-capacity self-observability for completed controller evaluations.

#![forbid(unsafe_code)]

use std::collections::VecDeque;

use winsched_control::EvaluationTelemetry;

pub const DEFAULT_EVALUATION_WINDOW_SAMPLES: usize = 60;

#[derive(Debug, Clone)]
pub struct EvaluationAccumulator {
    capacity: usize,
    completed_total: u64,
    durations_us: VecDeque<u64>,
    last_scanned_processes: usize,
    last_eligible_processes: usize,
    last_decisions: usize,
}

impl Default for EvaluationAccumulator {
    fn default() -> Self {
        Self::new(DEFAULT_EVALUATION_WINDOW_SAMPLES)
    }
}

impl EvaluationAccumulator {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            completed_total: 0,
            durations_us: VecDeque::with_capacity(capacity.max(1)),
            last_scanned_processes: 0,
            last_eligible_processes: 0,
            last_decisions: 0,
        }
    }

    pub fn record(
        &mut self,
        duration_us: u64,
        scanned_processes: usize,
        eligible_processes: usize,
        decisions: usize,
    ) {
        if self.durations_us.len() == self.capacity {
            self.durations_us.pop_front();
        }
        self.durations_us.push_back(duration_us);
        self.completed_total = self.completed_total.saturating_add(1);
        self.last_scanned_processes = scanned_processes;
        self.last_eligible_processes = eligible_processes;
        self.last_decisions = decisions;
    }

    #[must_use]
    pub fn status(&self) -> EvaluationTelemetry {
        let mut sorted = self.durations_us.iter().copied().collect::<Vec<_>>();
        sorted.sort_unstable();
        let sum = sorted.iter().fold(0u128, |total, &sample| {
            total.saturating_add(u128::from(sample))
        });
        let mean = if sorted.is_empty() {
            0
        } else {
            u64::try_from(sum / sorted.len() as u128).unwrap_or(u64::MAX)
        };
        EvaluationTelemetry {
            completed_total: self.completed_total,
            last_duration_us: self.durations_us.back().copied().unwrap_or(0),
            rolling_mean_us: mean,
            rolling_p95_us: nearest_rank(&sorted, 95),
            rolling_max_us: sorted.last().copied().unwrap_or(0),
            window_samples: sorted.len(),
            last_scanned_processes: self.last_scanned_processes,
            last_eligible_processes: self.last_eligible_processes,
            last_decisions: self.last_decisions,
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
    fn reports_fixed_window_mean_p95_max_and_population_counts() {
        let mut metrics = EvaluationAccumulator::new(4);
        for (duration, scanned, eligible, decisions) in [
            (100, 80, 10, 10),
            (200, 81, 11, 11),
            (300, 82, 12, 12),
            (400, 83, 13, 13),
            (500, 84, 14, 14),
        ] {
            metrics.record(duration, scanned, eligible, decisions);
        }
        let status = metrics.status();
        assert_eq!(status.completed_total, 5);
        assert_eq!(status.window_samples, 4);
        assert_eq!(status.last_duration_us, 500);
        assert_eq!(status.rolling_mean_us, 350);
        assert_eq!(status.rolling_p95_us, 500);
        assert_eq!(status.rolling_max_us, 500);
        assert_eq!(status.last_scanned_processes, 84);
        assert_eq!(status.last_eligible_processes, 14);
        assert_eq!(status.last_decisions, 14);
    }

    #[test]
    fn empty_and_single_sample_windows_are_well_defined() {
        let mut metrics = EvaluationAccumulator::new(0);
        assert_eq!(metrics.status(), EvaluationTelemetry::default());
        metrics.record(123, 4, 3, 2);
        let status = metrics.status();
        assert_eq!(status.completed_total, 1);
        assert_eq!(status.window_samples, 1);
        assert_eq!(status.rolling_mean_us, 123);
        assert_eq!(status.rolling_p95_us, 123);
        assert_eq!(status.rolling_max_us, 123);
    }
}
