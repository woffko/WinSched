//! Hysteresis and periodic coalescing for responsiveness telemetry.

#![forbid(unsafe_code)]

use winsched_core::responsiveness::ResponsivenessPressure;

pub const DEFAULT_RESPONSIVENESS_SUMMARY_INTERVAL_MS: u64 = 60_000;
const DEFAULT_TRANSITION_STABILITY_SAMPLES: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponsivenessSignature {
    pub pressure: ResponsivenessPressure,
    pub memory_profile_physical_cores: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponsivenessLogReason {
    Initial,
    Transition,
    Periodic,
}

impl ResponsivenessLogReason {
    #[must_use]
    #[cfg(windows)]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Transition => "transition",
            Self::Periodic => "periodic",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ResponsivenessLogGate {
    summary_interval_ms: u64,
    transition_stability_samples: u8,
    last_emitted_ms: Option<u64>,
    last_signature: Option<ResponsivenessSignature>,
    candidate_signature: Option<ResponsivenessSignature>,
    candidate_samples: u8,
}

impl Default for ResponsivenessLogGate {
    fn default() -> Self {
        Self::new(
            DEFAULT_RESPONSIVENESS_SUMMARY_INTERVAL_MS,
            DEFAULT_TRANSITION_STABILITY_SAMPLES,
        )
    }
}

impl ResponsivenessLogGate {
    #[must_use]
    pub const fn new(summary_interval_ms: u64, transition_stability_samples: u8) -> Self {
        Self {
            summary_interval_ms,
            transition_stability_samples: if transition_stability_samples == 0 {
                1
            } else {
                transition_stability_samples
            },
            last_emitted_ms: None,
            last_signature: None,
            candidate_signature: None,
            candidate_samples: 0,
        }
    }

    pub fn decide(
        &mut self,
        now_ms: u64,
        signature: ResponsivenessSignature,
        force_transition: bool,
    ) -> Option<ResponsivenessLogReason> {
        let Some(last_signature) = self.last_signature else {
            return Some(self.record(now_ms, signature, ResponsivenessLogReason::Initial));
        };
        if force_transition
            || signature.memory_profile_physical_cores
                != last_signature.memory_profile_physical_cores
        {
            return Some(self.record(now_ms, signature, ResponsivenessLogReason::Transition));
        }
        if signature == last_signature {
            self.candidate_signature = None;
            self.candidate_samples = 0;
        } else {
            if self.candidate_signature == Some(signature) {
                self.candidate_samples = self.candidate_samples.saturating_add(1);
            } else {
                self.candidate_signature = Some(signature);
                self.candidate_samples = 1;
            }
            if self.candidate_samples >= self.transition_stability_samples {
                return Some(self.record(now_ms, signature, ResponsivenessLogReason::Transition));
            }
        }
        if self
            .last_emitted_ms
            .is_some_and(|last| now_ms.saturating_sub(last) >= self.summary_interval_ms)
        {
            return Some(self.record(now_ms, signature, ResponsivenessLogReason::Periodic));
        }
        None
    }

    fn record(
        &mut self,
        now_ms: u64,
        signature: ResponsivenessSignature,
        reason: ResponsivenessLogReason,
    ) -> ResponsivenessLogReason {
        self.last_emitted_ms = Some(now_ms);
        self.last_signature = Some(signature);
        self.candidate_signature = None;
        self.candidate_samples = 0;
        reason
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature(pressure: ResponsivenessPressure, cores: u16) -> ResponsivenessSignature {
        ResponsivenessSignature {
            pressure,
            memory_profile_physical_cores: cores,
        }
    }

    #[test]
    fn steady_state_emits_once_per_minute() {
        let normal = signature(ResponsivenessPressure::Normal, 28);
        let mut gate = ResponsivenessLogGate::default();
        assert_eq!(
            gate.decide(0, normal, false),
            Some(ResponsivenessLogReason::Initial)
        );
        for now in 1_000..60_000 {
            assert_eq!(gate.decide(now, normal, false), None);
        }
        assert_eq!(
            gate.decide(60_000, normal, false),
            Some(ResponsivenessLogReason::Periodic)
        );
    }

    #[test]
    fn pressure_transition_requires_three_stable_samples() {
        let normal = signature(ResponsivenessPressure::Normal, 28);
        let elevated = signature(ResponsivenessPressure::Elevated, 28);
        let mut gate = ResponsivenessLogGate::default();
        assert!(gate.decide(0, normal, false).is_some());
        assert_eq!(gate.decide(1_000, elevated, false), None);
        assert_eq!(gate.decide(2_000, normal, false), None);
        assert_eq!(gate.decide(3_000, elevated, false), None);
        assert_eq!(gate.decide(4_000, elevated, false), None);
        assert_eq!(
            gate.decide(5_000, elevated, false),
            Some(ResponsivenessLogReason::Transition)
        );
    }

    #[test]
    fn width_change_emits_immediately() {
        let normal = signature(ResponsivenessPressure::Normal, 28);
        let shrunk = signature(ResponsivenessPressure::Elevated, 25);
        let mut gate = ResponsivenessLogGate::default();
        assert!(gate.decide(0, normal, false).is_some());
        assert_eq!(
            gate.decide(1_000, shrunk, true),
            Some(ResponsivenessLogReason::Transition)
        );
    }
}
