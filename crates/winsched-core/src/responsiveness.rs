//! Adaptive physical-core width for explicitly memory-bound workloads.

use serde::{Deserialize, Serialize};

use crate::latency::SchedulerLatencyStatus;

const MIN_LATENCY_SAMPLES: usize = 100;
const ELEVATED_DPC_OR_INTERRUPT_BPS: u16 = 500;
const RECOVERED_DPC_OR_INTERRUPT_BPS: u16 = 200;

/// Runtime inputs and bounds for memory-profile width control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdaptiveWidthConfig {
    pub enabled: bool,
    pub minimum_physical_cores: u16,
    pub maximum_physical_cores: u16,
    pub latency_target_p99_us: u64,
    pub latency_recovery_p99_us: u64,
    pub stability_samples: u16,
    pub resize_cooldown_ms: u64,
}

/// Current interpretation of scheduler and interrupt pressure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponsivenessPressure {
    #[default]
    Unknown,
    Normal,
    Elevated,
}

/// One bounded memory-profile width transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidthAdjustment {
    Shrunk { from: u16, to: u16 },
    Expanded { from: u16, to: u16 },
}

/// Stable, cooldown-protected memory-profile width controller.
#[derive(Debug, Clone)]
pub struct AdaptiveWidthController {
    config: AdaptiveWidthConfig,
    current_physical_cores: u16,
    elevated_streak: u16,
    recovered_streak: u16,
    last_adjustment_ms: Option<u64>,
    pressure: ResponsivenessPressure,
}

impl AdaptiveWidthController {
    #[must_use]
    pub const fn new(config: AdaptiveWidthConfig) -> Self {
        Self {
            current_physical_cores: config.maximum_physical_cores,
            config,
            elevated_streak: 0,
            recovered_streak: 0,
            last_adjustment_ms: None,
            pressure: ResponsivenessPressure::Unknown,
        }
    }

    pub fn reconfigure(&mut self, config: AdaptiveWidthConfig) {
        self.config = config;
        self.current_physical_cores = self
            .current_physical_cores
            .clamp(config.minimum_physical_cores, config.maximum_physical_cores);
        self.elevated_streak = 0;
        self.recovered_streak = 0;
        if !config.enabled {
            self.current_physical_cores = config.maximum_physical_cores;
            self.pressure = ResponsivenessPressure::Unknown;
            self.last_adjustment_ms = None;
        }
    }

    #[must_use]
    pub const fn current_physical_cores(&self) -> u16 {
        self.current_physical_cores
    }

    #[must_use]
    pub const fn pressure(&self) -> ResponsivenessPressure {
        self.pressure
    }

    pub fn evaluate(
        &mut self,
        now_ms: u64,
        latency: SchedulerLatencyStatus,
        maximum_dpc_time_bps: u16,
        maximum_interrupt_time_bps: u16,
    ) -> Option<WidthAdjustment> {
        if !self.config.enabled || latency.window_samples < MIN_LATENCY_SAMPLES {
            self.pressure = ResponsivenessPressure::Unknown;
            self.elevated_streak = 0;
            self.recovered_streak = 0;
            return None;
        }

        let elevated = latency.p99_lateness_us > self.config.latency_target_p99_us
            || maximum_dpc_time_bps > ELEVATED_DPC_OR_INTERRUPT_BPS
            || maximum_interrupt_time_bps > ELEVATED_DPC_OR_INTERRUPT_BPS;
        let recovered = latency.p99_lateness_us <= self.config.latency_recovery_p99_us
            && maximum_dpc_time_bps <= RECOVERED_DPC_OR_INTERRUPT_BPS
            && maximum_interrupt_time_bps <= RECOVERED_DPC_OR_INTERRUPT_BPS;

        if elevated {
            self.pressure = ResponsivenessPressure::Elevated;
            self.elevated_streak = self.elevated_streak.saturating_add(1);
            self.recovered_streak = 0;
        } else if recovered {
            self.pressure = ResponsivenessPressure::Normal;
            self.recovered_streak = self.recovered_streak.saturating_add(1);
            self.elevated_streak = 0;
        } else {
            self.pressure = ResponsivenessPressure::Normal;
            self.elevated_streak = 0;
            self.recovered_streak = 0;
        }

        if self
            .last_adjustment_ms
            .is_some_and(|last| now_ms.saturating_sub(last) < self.config.resize_cooldown_ms)
        {
            return None;
        }
        if self.elevated_streak >= self.config.stability_samples
            && self.current_physical_cores > self.config.minimum_physical_cores
        {
            let from = self.current_physical_cores;
            let step = from.div_ceil(10).max(1);
            let to = from
                .saturating_sub(step)
                .max(self.config.minimum_physical_cores);
            self.current_physical_cores = to;
            self.elevated_streak = 0;
            self.last_adjustment_ms = Some(now_ms);
            return Some(WidthAdjustment::Shrunk { from, to });
        }
        if self.recovered_streak >= self.config.stability_samples
            && self.current_physical_cores < self.config.maximum_physical_cores
        {
            let from = self.current_physical_cores;
            let to = from
                .saturating_add(1)
                .min(self.config.maximum_physical_cores);
            self.current_physical_cores = to;
            self.recovered_streak = 0;
            self.last_adjustment_ms = Some(now_ms);
            return Some(WidthAdjustment::Expanded { from, to });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> AdaptiveWidthConfig {
        AdaptiveWidthConfig {
            enabled: true,
            minimum_physical_cores: 8,
            maximum_physical_cores: 28,
            latency_target_p99_us: 2_000,
            latency_recovery_p99_us: 1_000,
            stability_samples: 3,
            resize_cooldown_ms: 300_000,
        }
    }

    fn latency(p99_lateness_us: u64) -> SchedulerLatencyStatus {
        SchedulerLatencyStatus {
            enabled: true,
            window_samples: 100,
            p99_lateness_us,
            ..SchedulerLatencyStatus::default()
        }
    }

    #[test]
    fn sustained_latency_pressure_shrinks_by_ten_percent() {
        let mut controller = AdaptiveWidthController::new(config());
        assert_eq!(controller.evaluate(1_000, latency(3_000), 0, 0), None);
        assert_eq!(controller.evaluate(2_000, latency(3_000), 0, 0), None);
        assert_eq!(
            controller.evaluate(3_000, latency(3_000), 0, 0),
            Some(WidthAdjustment::Shrunk { from: 28, to: 25 })
        );
        assert_eq!(controller.pressure(), ResponsivenessPressure::Elevated);
    }

    #[test]
    fn cooldown_blocks_oscillation_and_recovery_expands_slowly() {
        let mut controller = AdaptiveWidthController::new(config());
        for now in [1_000, 2_000, 3_000] {
            let _ = controller.evaluate(now, latency(3_000), 0, 0);
        }
        assert_eq!(controller.current_physical_cores(), 25);
        for now in [4_000, 5_000, 6_000] {
            assert_eq!(controller.evaluate(now, latency(500), 0, 0), None);
        }
        assert_eq!(
            controller.evaluate(303_000, latency(500), 0, 0),
            Some(WidthAdjustment::Expanded { from: 25, to: 26 })
        );
    }

    #[test]
    fn incomplete_latency_window_never_changes_width() {
        let mut controller = AdaptiveWidthController::new(config());
        let incomplete = SchedulerLatencyStatus {
            enabled: true,
            window_samples: 99,
            p99_lateness_us: 50_000,
            ..SchedulerLatencyStatus::default()
        };
        for now in 0..10 {
            assert_eq!(controller.evaluate(now, incomplete, 10_000, 10_000), None);
        }
        assert_eq!(controller.current_physical_cores(), 28);
        assert_eq!(controller.pressure(), ResponsivenessPressure::Unknown);
    }
}
