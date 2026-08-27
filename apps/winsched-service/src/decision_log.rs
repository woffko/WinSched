//! Bounded aggregation for high-frequency placement decisions.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use winsched_core::adaptive::{
    DecisionReason, ExclusionReason, PolicyAction, PolicyDecision, ProcessKey,
};

pub const DEFAULT_DECISION_SUMMARY_INTERVAL_MS: u64 = 60_000;
pub const MAX_DECISION_SUMMARY_UNIQUE_PROCESSES: usize = 4_096;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct DecisionActionCounts {
    pub ignore: u64,
    pub keep: u64,
    pub assign: u64,
    #[serde(rename = "move")]
    pub move_process: u64,
    pub clear: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionReasonCount {
    pub reason: &'static str,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionLogSummary {
    pub window_duration_ms: u64,
    pub decisions: u64,
    pub unique_processes: usize,
    pub unique_processes_saturated: bool,
    pub enforcement_requested: u64,
    pub actions: DecisionActionCounts,
    pub reasons: Vec<DecisionReasonCount>,
}

#[derive(Debug, Clone)]
pub struct DecisionSummaryGate {
    summary_interval_ms: u64,
    window_started_ms: Option<u64>,
    decisions: u64,
    unique_processes: BTreeSet<ProcessKey>,
    unique_processes_saturated: bool,
    enforcement_requested: u64,
    actions: DecisionActionCounts,
    reasons: BTreeMap<&'static str, u64>,
}

impl Default for DecisionSummaryGate {
    fn default() -> Self {
        Self::new(DEFAULT_DECISION_SUMMARY_INTERVAL_MS)
    }
}

impl DecisionSummaryGate {
    #[must_use]
    pub const fn new(summary_interval_ms: u64) -> Self {
        Self {
            summary_interval_ms: if summary_interval_ms == 0 {
                1
            } else {
                summary_interval_ms
            },
            window_started_ms: None,
            decisions: 0,
            unique_processes: BTreeSet::new(),
            unique_processes_saturated: false,
            enforcement_requested: 0,
            actions: DecisionActionCounts {
                ignore: 0,
                keep: 0,
                assign: 0,
                move_process: 0,
                clear: 0,
            },
            reasons: BTreeMap::new(),
        }
    }

    /// Records one decision and returns the completed preceding window, if due.
    pub fn observe(
        &mut self,
        now_ms: u64,
        decision: &PolicyDecision,
    ) -> Option<DecisionLogSummary> {
        let summary = self.take_if_due(now_ms);
        self.window_started_ms.get_or_insert(now_ms);
        self.decisions = self.decisions.saturating_add(1);
        if self.unique_processes.contains(&decision.process) {
            // Already represented in the bounded cardinality set.
        } else if self.unique_processes.len() < MAX_DECISION_SUMMARY_UNIQUE_PROCESSES {
            self.unique_processes.insert(decision.process);
        } else {
            self.unique_processes_saturated = true;
        }
        if decision.enforce {
            self.enforcement_requested = self.enforcement_requested.saturating_add(1);
        }
        match decision.action {
            PolicyAction::Ignore => {
                self.actions.ignore = self.actions.ignore.saturating_add(1);
            }
            PolicyAction::Keep { .. } => {
                self.actions.keep = self.actions.keep.saturating_add(1);
            }
            PolicyAction::Assign { .. } => {
                self.actions.assign = self.actions.assign.saturating_add(1);
            }
            PolicyAction::Move { .. } => {
                self.actions.move_process = self.actions.move_process.saturating_add(1);
            }
            PolicyAction::Clear { .. } => {
                self.actions.clear = self.actions.clear.saturating_add(1);
            }
        }
        let reason = reason_name(decision.reason);
        let count = self.reasons.entry(reason).or_default();
        *count = count.saturating_add(1);
        summary
    }

    /// Flushes a partial non-empty window for shutdown or logging reconfiguration.
    pub fn flush(&mut self, now_ms: u64) -> Option<DecisionLogSummary> {
        self.take(now_ms)
    }

    /// Flushes a non-empty window only after its configured interval elapsed.
    pub fn flush_if_due(&mut self, now_ms: u64) -> Option<DecisionLogSummary> {
        self.take_if_due(now_ms)
    }

    pub fn reset(&mut self) {
        self.clear();
        self.window_started_ms = None;
    }

    fn take_if_due(&mut self, now_ms: u64) -> Option<DecisionLogSummary> {
        let started = self.window_started_ms?;
        if now_ms < started {
            self.window_started_ms = Some(now_ms);
            return None;
        }
        if now_ms.saturating_sub(started) < self.summary_interval_ms {
            return None;
        }
        self.take(now_ms)
    }

    fn take(&mut self, now_ms: u64) -> Option<DecisionLogSummary> {
        if self.decisions == 0 {
            self.window_started_ms = None;
            return None;
        }
        let started = self.window_started_ms.unwrap_or(now_ms);
        let reasons = self
            .reasons
            .iter()
            .map(|(&reason, &count)| DecisionReasonCount { reason, count })
            .collect();
        let summary = DecisionLogSummary {
            window_duration_ms: now_ms.saturating_sub(started),
            decisions: self.decisions,
            unique_processes: self.unique_processes.len(),
            unique_processes_saturated: self.unique_processes_saturated,
            enforcement_requested: self.enforcement_requested,
            actions: self.actions,
            reasons,
        };
        self.clear();
        self.window_started_ms = Some(now_ms);
        Some(summary)
    }

    fn clear(&mut self) {
        self.decisions = 0;
        self.unique_processes.clear();
        self.unique_processes_saturated = false;
        self.enforcement_requested = 0;
        self.actions = DecisionActionCounts::default();
        self.reasons.clear();
    }
}

const fn reason_name(reason: DecisionReason) -> &'static str {
    match reason {
        DecisionReason::ModeOff => "mode_off",
        DecisionReason::Excluded(ExclusionReason::SystemProcess) => "excluded_system_process",
        DecisionReason::Excluded(ExclusionReason::SessionZero) => "excluded_session_zero",
        DecisionReason::Excluded(ExclusionReason::ProtectedProcess) => "excluded_protected_process",
        DecisionReason::Excluded(ExclusionReason::RealtimeProcess) => "excluded_realtime_process",
        DecisionReason::Excluded(ExclusionReason::ExplicitRule) => "excluded_explicit_rule",
        DecisionReason::ExternalAssignment => "external_assignment",
        DecisionReason::PendingMutation => "pending_mutation",
        DecisionReason::PartitionRefresh => "partition_refresh",
        DecisionReason::ProfilePartition => "profile_partition",
        DecisionReason::ProfilePartitionStable => "profile_partition_stable",
        DecisionReason::InitialPlacement => "initial_placement",
        DecisionReason::StickyPlacement => "sticky_placement",
        DecisionReason::BelowOverloadThreshold => "below_overload_threshold",
        DecisionReason::StabilityWindow => "stability_window",
        DecisionReason::MinimumResidency => "minimum_residency",
        DecisionReason::Cooldown => "cooldown",
        DecisionReason::InsufficientImprovement => "insufficient_improvement",
        DecisionReason::NoAlternativeDomain => "no_alternative_domain",
        DecisionReason::BetterDomain => "better_domain",
        DecisionReason::StrictPlacement => "strict_placement",
        DecisionReason::AlreadyStrict => "already_strict",
        DecisionReason::RateLimited => "rate_limited",
    }
}

#[cfg(test)]
mod tests {
    use winsched_core::LlcDomainKey;

    use super::*;

    fn decision(
        pid: u32,
        action: PolicyAction,
        reason: DecisionReason,
        enforce: bool,
    ) -> PolicyDecision {
        PolicyDecision {
            process: ProcessKey {
                pid,
                creation_time_100ns: u64::from(pid) * 10,
            },
            action,
            reason,
            enforce,
        }
    }

    fn domain(index: u8) -> LlcDomainKey {
        LlcDomainKey {
            group: 0,
            last_level_cache_index: index,
        }
    }

    #[test]
    fn steady_decisions_emit_one_summary_at_the_interval_boundary() {
        let mut gate = DecisionSummaryGate::new(60_000);
        let keep = decision(
            10,
            PolicyAction::Keep {
                domain: Some(domain(0)),
            },
            DecisionReason::BelowOverloadThreshold,
            false,
        );
        assert_eq!(gate.observe(0, &keep), None);
        assert_eq!(gate.observe(59_999, &keep), None);
        let summary = gate.observe(60_000, &keep).unwrap();
        assert_eq!(summary.window_duration_ms, 60_000);
        assert_eq!(summary.decisions, 2);
        assert_eq!(summary.unique_processes, 1);
        assert_eq!(summary.actions.keep, 2);
        assert_eq!(
            summary.reasons,
            vec![DecisionReasonCount {
                reason: "below_overload_threshold",
                count: 2,
            }]
        );
        assert_eq!(gate.flush(60_001).unwrap().decisions, 1);
        assert_eq!(gate.flush(60_002), None);
    }

    #[test]
    fn due_flush_emits_without_requiring_another_process_decision() {
        let mut gate = DecisionSummaryGate::new(60_000);
        let keep = decision(
            10,
            PolicyAction::Keep { domain: None },
            DecisionReason::BelowOverloadThreshold,
            false,
        );
        assert_eq!(gate.observe(1_000, &keep), None);
        assert_eq!(gate.flush_if_due(60_999), None);
        let summary = gate.flush_if_due(61_000).unwrap();
        assert_eq!(summary.window_duration_ms, 60_000);
        assert_eq!(summary.decisions, 1);
        assert_eq!(gate.flush_if_due(121_000), None);
    }

    #[test]
    fn summary_counts_actions_reasons_enforcement_and_unique_processes() {
        let mut gate = DecisionSummaryGate::new(60_000);
        let assign = decision(
            10,
            PolicyAction::Assign {
                target: domain(8),
                cpu_set_ids: vec![1, 2],
            },
            DecisionReason::InitialPlacement,
            true,
        );
        let ignored = decision(
            20,
            PolicyAction::Ignore,
            DecisionReason::Excluded(ExclusionReason::SessionZero),
            false,
        );
        assert_eq!(gate.observe(1_000, &assign), None);
        assert_eq!(gate.observe(2_000, &ignored), None);
        assert_eq!(gate.observe(3_000, &assign), None);
        let summary = gate.flush(4_000).unwrap();
        assert_eq!(summary.decisions, 3);
        assert_eq!(summary.unique_processes, 2);
        assert_eq!(summary.enforcement_requested, 2);
        assert_eq!(summary.actions.assign, 2);
        assert_eq!(summary.actions.ignore, 1);
        assert_eq!(
            summary.reasons,
            vec![
                DecisionReasonCount {
                    reason: "excluded_session_zero",
                    count: 1,
                },
                DecisionReasonCount {
                    reason: "initial_placement",
                    count: 2,
                },
            ]
        );
    }

    #[test]
    fn empty_and_reset_windows_do_not_emit() {
        let mut gate = DecisionSummaryGate::default();
        assert_eq!(gate.flush(1_000), None);
        let keep = decision(
            1,
            PolicyAction::Keep { domain: None },
            DecisionReason::ModeOff,
            false,
        );
        assert_eq!(gate.observe(2_000, &keep), None);
        gate.reset();
        assert_eq!(gate.flush(100_000), None);
    }

    #[test]
    fn monotonic_regression_starts_a_fresh_bounded_window() {
        let mut gate = DecisionSummaryGate::new(100);
        let keep = decision(
            1,
            PolicyAction::Keep { domain: None },
            DecisionReason::Cooldown,
            false,
        );
        assert_eq!(gate.observe(1_000, &keep), None);
        assert_eq!(gate.observe(900, &keep), None);
        assert_eq!(gate.observe(999, &keep), None);
        assert_eq!(gate.observe(1_000, &keep).unwrap().decisions, 3);
    }

    #[test]
    fn high_process_count_coalesces_to_one_bounded_minute_summary() {
        let mut gate = DecisionSummaryGate::default();
        let mut emitted = Vec::new();
        for second in 0..=60u64 {
            for pid in 1..=82 {
                let keep = decision(
                    pid,
                    PolicyAction::Keep {
                        domain: Some(domain(8)),
                    },
                    DecisionReason::BelowOverloadThreshold,
                    false,
                );
                if let Some(summary) = gate.observe(second * 1_000, &keep) {
                    emitted.push(summary);
                }
            }
        }
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].decisions, 60 * 82);
        assert_eq!(emitted[0].unique_processes, 82);
        assert_eq!(emitted[0].actions.keep, 60 * 82);
        assert_eq!(gate.flush(60_001).unwrap().decisions, 82);
    }

    #[test]
    fn unique_process_tracking_has_an_explicit_fixed_capacity() {
        let mut gate = DecisionSummaryGate::new(60_000);
        for pid in 1..=u32::try_from(MAX_DECISION_SUMMARY_UNIQUE_PROCESSES + 1).unwrap() {
            let keep = decision(
                pid,
                PolicyAction::Keep { domain: None },
                DecisionReason::BelowOverloadThreshold,
                false,
            );
            assert_eq!(gate.observe(0, &keep), None);
        }
        let summary = gate.flush(1).unwrap();
        assert_eq!(
            summary.unique_processes,
            MAX_DECISION_SUMMARY_UNIQUE_PROCESSES
        );
        assert!(summary.unique_processes_saturated);
    }
}
