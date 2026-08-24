//! Stateful, side-effect-free placement decisions for the future service.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{CpuPartition, LlcDomain, LlcDomainKey, ProcessorClassPreference, Topology};

const FULL_UTILIZATION_BPS: u16 = 10_000;

/// Tuning values for the adaptive placement policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyConfig {
    pub overload_threshold_bps: u16,
    pub minimum_improvement_bps: u16,
    pub stability_samples: u16,
    pub minimum_residency_ms: u64,
    pub cooldown_ms: u64,
    pub max_mutations_per_evaluation: u16,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            overload_threshold_bps: 8_500,
            minimum_improvement_bps: 2_000,
            stability_samples: 3,
            minimum_residency_ms: 10_000,
            cooldown_ms: 30_000,
            max_mutations_per_evaluation: 1,
        }
    }
}

impl PolicyConfig {
    /// Validates all policy bounds.
    ///
    /// # Errors
    ///
    /// Returns [`AdaptiveError::InvalidConfig`] for an unusable threshold,
    /// zero stability window, or zero mutation budget.
    pub fn validate(self) -> Result<Self, AdaptiveError> {
        if self.overload_threshold_bps > FULL_UTILIZATION_BPS {
            return Err(AdaptiveError::InvalidConfig(
                "overload_threshold_bps must be <= 10000",
            ));
        }
        if self.minimum_improvement_bps > FULL_UTILIZATION_BPS {
            return Err(AdaptiveError::InvalidConfig(
                "minimum_improvement_bps must be <= 10000",
            ));
        }
        if self.stability_samples == 0 {
            return Err(AdaptiveError::InvalidConfig(
                "stability_samples must be greater than zero",
            ));
        }
        if self.max_mutations_per_evaluation == 0 {
            return Err(AdaptiveError::InvalidConfig(
                "max_mutations_per_evaluation must be greater than zero",
            ));
        }
        Ok(self)
    }
}

/// PID plus creation time, preventing PID reuse from inheriting policy state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProcessKey {
    pub pid: u32,
    pub creation_time_100ns: u64,
}

/// User-visible policy mode for a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlacementMode {
    Off,
    Sticky,
    Auto,
    Performance,
    Efficiency,
    Strict(LlcDomainKey),
}

/// Whether a policy decision may be applied to Windows or only reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnforcementMode {
    Observe,
    Apply,
}

/// Who owns the CPU Set assignment observed on the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssignmentOrigin {
    None,
    Managed,
    External,
}

/// A safety reason that prevents automatic control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExclusionReason {
    SystemProcess,
    SessionZero,
    ProtectedProcess,
    RealtimeProcess,
    ExplicitRule,
}

/// One process as observed by the platform layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessObservation {
    pub key: ProcessKey,
    pub mode: PlacementMode,
    pub enforcement: EnforcementMode,
    pub current_domain: Option<LlcDomainKey>,
    pub assignment_origin: AssignmentOrigin,
    pub refresh_required: bool,
    pub preferred_partition: Option<CpuPartition>,
    pub exclusion: Option<ExclusionReason>,
}

/// Aggregate utilization of one LLC domain, in basis points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainLoad {
    pub domain: LlcDomainKey,
    pub utilization_bps: u16,
    pub dpc_time_bps: u16,
    pub interrupt_time_bps: u16,
}

/// A mutation or no-op recommended by the policy engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyAction {
    Ignore,
    Keep {
        domain: Option<LlcDomainKey>,
    },
    Assign {
        target: LlcDomainKey,
        cpu_set_ids: Vec<u32>,
    },
    Move {
        source: LlcDomainKey,
        target: LlcDomainKey,
        cpu_set_ids: Vec<u32>,
    },
    Clear {
        source: LlcDomainKey,
    },
}

impl PolicyAction {
    #[must_use]
    pub const fn is_mutation(&self) -> bool {
        matches!(
            self,
            Self::Assign { .. } | Self::Move { .. } | Self::Clear { .. }
        )
    }

    const fn pending_mutation(&self) -> Option<PendingMutation> {
        match self {
            Self::Assign { target, .. } | Self::Move { target, .. } => {
                Some(PendingMutation::Set(*target))
            }
            Self::Clear { .. } => Some(PendingMutation::Clear),
            Self::Ignore | Self::Keep { .. } => None,
        }
    }
}

/// Machine-readable explanation for a policy decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecisionReason {
    ModeOff,
    Excluded(ExclusionReason),
    ExternalAssignment,
    PendingMutation,
    PartitionRefresh,
    ProfilePartition,
    ProfilePartitionStable,
    InitialPlacement,
    StickyPlacement,
    BelowOverloadThreshold,
    StabilityWindow,
    MinimumResidency,
    Cooldown,
    InsufficientImprovement,
    NoAlternativeDomain,
    BetterDomain,
    StrictPlacement,
    AlreadyStrict,
    RateLimited,
}

/// One complete decision emitted for logging or enforcement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub process: ProcessKey,
    pub action: PolicyAction,
    pub reason: DecisionReason,
    pub enforce: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AdaptiveError {
    #[error("invalid policy configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("duplicate load sample for group={0}, llc={1}")]
    DuplicateDomainLoad(u16, u8),
    #[error("load sample references unknown domain group={0}, llc={1}")]
    UnknownLoadDomain(u16, u8),
    #[error("missing load sample for group={0}, llc={1}")]
    MissingDomainLoad(u16, u8),
    #[error("process references unknown domain group={0}, llc={1}")]
    UnknownProcessDomain(u16, u8),
    #[error("domain group={0}, llc={1} has no assignable CPU Sets")]
    NoAssignableCpuSets(u16, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingMutation {
    Set(LlcDomainKey),
    Clear,
}

impl PendingMutation {
    const fn target(self) -> Option<LlcDomainKey> {
        match self {
            Self::Set(target) => Some(target),
            Self::Clear => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessState {
    current_domain: Option<LlcDomainKey>,
    observed_domain: Option<LlcDomainKey>,
    virtual_domain: Option<LlcDomainKey>,
    domain_since_ms: u64,
    last_move_ms: Option<u64>,
    overload_streak: u16,
    pending: Option<PendingMutation>,
}

impl ProcessState {
    const fn new(current_domain: Option<LlcDomainKey>, now_ms: u64) -> Self {
        Self {
            current_domain,
            observed_domain: current_domain,
            virtual_domain: current_domain,
            domain_since_ms: now_ms,
            last_move_ms: None,
            overload_streak: 0,
            pending: None,
        }
    }

    fn reconcile(&mut self, observed: Option<LlcDomainKey>, now_ms: u64) {
        if self.current_domain != observed {
            self.current_domain = observed;
            self.observed_domain = observed;
            self.virtual_domain = None;
            self.domain_since_ms = now_ms;
            self.overload_streak = 0;
            self.pending = None;
        }
    }

    fn reconcile_observe(&mut self, observed: Option<LlcDomainKey>, now_ms: u64) {
        if self.observed_domain != observed {
            self.observed_domain = observed;
            self.virtual_domain = observed;
            self.domain_since_ms = now_ms;
            self.last_move_ms = None;
            self.overload_streak = 0;
            self.pending = None;
        }
    }

    fn simulate(&mut self, action: &PolicyAction, now_ms: u64) {
        let Some(target) = action.pending_mutation().map(PendingMutation::target) else {
            return;
        };
        if self.virtual_domain != target {
            self.last_move_ms = Some(now_ms);
        }
        self.virtual_domain = target;
        self.domain_since_ms = now_ms;
        self.overload_streak = 0;
    }
}

/// Stateful coordinator that computes decisions but never calls operating-system APIs.
#[derive(Debug)]
pub struct PolicyEngine {
    config: PolicyConfig,
    states: BTreeMap<ProcessKey, ProcessState>,
}

impl PolicyEngine {
    /// Creates a validated policy engine.
    ///
    /// # Errors
    ///
    /// Returns [`AdaptiveError::InvalidConfig`] when a policy bound is invalid.
    pub fn new(config: PolicyConfig) -> Result<Self, AdaptiveError> {
        Ok(Self {
            config: config.validate()?,
            states: BTreeMap::new(),
        })
    }

    /// Evaluates a complete observation snapshot.
    ///
    /// The caller must invoke [`Self::acknowledge`] after attempting each
    /// decision whose `enforce` field is true.
    ///
    /// # Errors
    ///
    /// Returns an [`AdaptiveError`] for invalid, incomplete, or inconsistent
    /// topology and load observations.
    pub fn evaluate(
        &mut self,
        now_ms: u64,
        topology: &Topology,
        loads: &[DomainLoad],
        processes: &[ProcessObservation],
    ) -> Result<Vec<PolicyDecision>, AdaptiveError> {
        let load_map = validate_loads(topology, loads)?;
        let live = processes
            .iter()
            .map(|process| process.key)
            .collect::<BTreeSet<_>>();
        self.states.retain(|key, _| live.contains(key));

        let mut decisions = Vec::with_capacity(processes.len());
        let mut mutations = 0u16;
        let config = self.config;

        for process in processes {
            validate_process_domain(topology, process.current_domain)?;
            let state = self
                .states
                .entry(process.key)
                .or_insert_with(|| ProcessState::new(process.current_domain, now_ms));
            let effective_process = if process.enforcement == EnforcementMode::Observe {
                state.reconcile_observe(process.current_domain, now_ms);
                let mut effective = process.clone();
                effective.current_domain = state.virtual_domain;
                effective
            } else {
                state.reconcile(process.current_domain, now_ms);
                state.virtual_domain = None;
                process.clone()
            };

            let mut decision = evaluate_process(
                config,
                now_ms,
                topology,
                &load_map,
                &effective_process,
                state,
            )?;

            if decision.action.is_mutation() {
                if !decision.enforce {
                    state.simulate(&decision.action, now_ms);
                    decisions.push(decision);
                    continue;
                }
                if mutations >= config.max_mutations_per_evaluation {
                    decision = PolicyDecision {
                        process: process.key,
                        action: PolicyAction::Keep {
                            domain: effective_process.current_domain,
                        },
                        reason: DecisionReason::RateLimited,
                        enforce: false,
                    };
                } else {
                    mutations += 1;
                    state.pending = decision.action.pending_mutation();
                }
            }
            decisions.push(decision);
        }

        Ok(decisions)
    }

    /// Confirms whether one pending operating-system mutation succeeded.
    ///
    /// Returns `false` when the process has no matching pending mutation.
    pub fn acknowledge(
        &mut self,
        process: ProcessKey,
        target: Option<LlcDomainKey>,
        succeeded: bool,
        now_ms: u64,
    ) -> bool {
        let Some(state) = self.states.get_mut(&process) else {
            return false;
        };
        let Some(pending) = state.pending else {
            return false;
        };
        if pending.target() != target {
            return false;
        }

        state.pending = None;
        if succeeded {
            if state.current_domain != target {
                state.last_move_ms = Some(now_ms);
            }
            state.current_domain = target;
            state.observed_domain = target;
            state.virtual_domain = None;
            state.domain_since_ms = now_ms;
            state.overload_streak = 0;
        }
        true
    }
}

fn validate_loads(
    topology: &Topology,
    loads: &[DomainLoad],
) -> Result<BTreeMap<LlcDomainKey, u16>, AdaptiveError> {
    let known = topology
        .llc_domains
        .iter()
        .map(|domain| domain.key)
        .collect::<BTreeSet<_>>();
    let mut result = BTreeMap::new();
    for load in loads {
        if !known.contains(&load.domain) {
            return Err(AdaptiveError::UnknownLoadDomain(
                load.domain.group,
                load.domain.last_level_cache_index,
            ));
        }
        if result
            .insert(load.domain, load.utilization_bps.min(FULL_UTILIZATION_BPS))
            .is_some()
        {
            return Err(AdaptiveError::DuplicateDomainLoad(
                load.domain.group,
                load.domain.last_level_cache_index,
            ));
        }
    }
    for domain in &topology.llc_domains {
        if !result.contains_key(&domain.key) {
            return Err(AdaptiveError::MissingDomainLoad(
                domain.key.group,
                domain.key.last_level_cache_index,
            ));
        }
    }
    Ok(result)
}

fn validate_process_domain(
    topology: &Topology,
    current: Option<LlcDomainKey>,
) -> Result<(), AdaptiveError> {
    if let Some(current) = current
        && !topology
            .llc_domains
            .iter()
            .any(|domain| domain.key == current)
    {
        return Err(AdaptiveError::UnknownProcessDomain(
            current.group,
            current.last_level_cache_index,
        ));
    }
    Ok(())
}

fn evaluate_process(
    config: PolicyConfig,
    now_ms: u64,
    topology: &Topology,
    loads: &BTreeMap<LlcDomainKey, u16>,
    process: &ProcessObservation,
    state: &mut ProcessState,
) -> Result<PolicyDecision, AdaptiveError> {
    if let Some(exclusion) = process.exclusion {
        return Ok(decision(
            process,
            PolicyAction::Ignore,
            DecisionReason::Excluded(exclusion),
            false,
        ));
    }

    if process.mode == PlacementMode::Off {
        return if process.assignment_origin == AssignmentOrigin::Managed {
            Ok(decision(
                process,
                process
                    .current_domain
                    .map_or(PolicyAction::Ignore, |source| PolicyAction::Clear {
                        source,
                    }),
                DecisionReason::ModeOff,
                true,
            ))
        } else {
            Ok(decision(
                process,
                PolicyAction::Ignore,
                DecisionReason::ModeOff,
                false,
            ))
        };
    }

    if process.assignment_origin == AssignmentOrigin::External
        && !matches!(process.mode, PlacementMode::Strict(_))
    {
        return Ok(decision(
            process,
            PolicyAction::Ignore,
            DecisionReason::ExternalAssignment,
            false,
        ));
    }

    if state.pending.is_some() {
        return Ok(decision(
            process,
            PolicyAction::Keep {
                domain: process.current_domain,
            },
            DecisionReason::PendingMutation,
            false,
        ));
    }

    let enforce = process.enforcement == EnforcementMode::Apply;
    let class = class_preference(process.mode);

    if let Some(profile_decision) = preferred_partition_decision(process, enforce) {
        return Ok(profile_decision);
    }

    if process.refresh_required
        && let Some(target) = process.current_domain
    {
        let cpu_set_ids = domain_cpu_set_ids(topology, target, class)?;
        return Ok(decision(
            process,
            PolicyAction::Assign {
                target,
                cpu_set_ids,
            },
            DecisionReason::PartitionRefresh,
            enforce,
        ));
    }

    if let PlacementMode::Strict(target) = process.mode {
        return Ok(strict_decision(topology, process, target, class));
    }

    let Some(current) = process.current_domain else {
        return Ok(initial_assignment_decision(
            topology, loads, process, class, enforce,
        ));
    };

    if process.mode == PlacementMode::Sticky {
        return Ok(decision(
            process,
            PolicyAction::Keep {
                domain: Some(current),
            },
            DecisionReason::StickyPlacement,
            false,
        ));
    }

    Ok(adaptive_existing_decision(
        config, now_ms, topology, loads, process, state, current, class, enforce,
    ))
}

fn initial_assignment_decision(
    topology: &Topology,
    loads: &BTreeMap<LlcDomainKey, u16>,
    process: &ProcessObservation,
    class: ProcessorClassPreference,
    enforce: bool,
) -> PolicyDecision {
    let Ok((target, cpu_set_ids)) = least_loaded_domain(topology, loads, None, class) else {
        return decision(
            process,
            PolicyAction::Ignore,
            DecisionReason::NoAlternativeDomain,
            false,
        );
    };
    decision(
        process,
        PolicyAction::Assign {
            target,
            cpu_set_ids,
        },
        DecisionReason::InitialPlacement,
        enforce,
    )
}

fn preferred_partition_decision(
    process: &ProcessObservation,
    enforce: bool,
) -> Option<PolicyDecision> {
    let partition = process.preferred_partition.as_ref()?;
    if process.assignment_origin == AssignmentOrigin::Managed && !process.refresh_required {
        return Some(decision(
            process,
            PolicyAction::Keep {
                domain: Some(partition.anchor_domain),
            },
            DecisionReason::ProfilePartitionStable,
            false,
        ));
    }
    Some(decision(
        process,
        PolicyAction::Assign {
            target: partition.anchor_domain,
            cpu_set_ids: partition.cpu_set_ids.clone(),
        },
        if process.assignment_origin == AssignmentOrigin::Managed {
            DecisionReason::PartitionRefresh
        } else {
            DecisionReason::ProfilePartition
        },
        enforce,
    ))
}

fn strict_decision(
    topology: &Topology,
    process: &ProcessObservation,
    target: LlcDomainKey,
    class: ProcessorClassPreference,
) -> PolicyDecision {
    let Ok(cpu_set_ids) = domain_cpu_set_ids(topology, target, class) else {
        return decision(
            process,
            PolicyAction::Keep {
                domain: process.current_domain,
            },
            DecisionReason::NoAlternativeDomain,
            false,
        );
    };
    if process.current_domain == Some(target) {
        decision(
            process,
            PolicyAction::Keep {
                domain: Some(target),
            },
            DecisionReason::AlreadyStrict,
            false,
        )
    } else {
        decision(
            process,
            process.current_domain.map_or(
                PolicyAction::Assign {
                    target,
                    cpu_set_ids: cpu_set_ids.clone(),
                },
                |source| PolicyAction::Move {
                    source,
                    target,
                    cpu_set_ids,
                },
            ),
            DecisionReason::StrictPlacement,
            true,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn adaptive_existing_decision(
    config: PolicyConfig,
    now_ms: u64,
    topology: &Topology,
    loads: &BTreeMap<LlcDomainKey, u16>,
    process: &ProcessObservation,
    state: &mut ProcessState,
    current: LlcDomainKey,
    class: ProcessorClassPreference,
    enforce: bool,
) -> PolicyDecision {
    let current_load = loads[&current];
    if current_load < config.overload_threshold_bps {
        state.overload_streak = 0;
        return decision(
            process,
            PolicyAction::Keep {
                domain: Some(current),
            },
            DecisionReason::BelowOverloadThreshold,
            false,
        );
    }

    state.overload_streak = state.overload_streak.saturating_add(1);
    if state.overload_streak < config.stability_samples {
        return decision(
            process,
            PolicyAction::Keep {
                domain: Some(current),
            },
            DecisionReason::StabilityWindow,
            false,
        );
    }

    if now_ms.saturating_sub(state.domain_since_ms) < config.minimum_residency_ms {
        return decision(
            process,
            PolicyAction::Keep {
                domain: Some(current),
            },
            DecisionReason::MinimumResidency,
            false,
        );
    }

    if state
        .last_move_ms
        .is_some_and(|last| now_ms.saturating_sub(last) < config.cooldown_ms)
    {
        return decision(
            process,
            PolicyAction::Keep {
                domain: Some(current),
            },
            DecisionReason::Cooldown,
            false,
        );
    }

    let alternative = least_loaded_domain(topology, loads, Some(current), class);
    let Ok((target, cpu_set_ids)) = alternative else {
        return decision(
            process,
            PolicyAction::Keep {
                domain: Some(current),
            },
            DecisionReason::NoAlternativeDomain,
            false,
        );
    };
    let improvement = current_load.saturating_sub(loads[&target]);
    if improvement < config.minimum_improvement_bps {
        return decision(
            process,
            PolicyAction::Keep {
                domain: Some(current),
            },
            DecisionReason::InsufficientImprovement,
            false,
        );
    }

    decision(
        process,
        PolicyAction::Move {
            source: current,
            target,
            cpu_set_ids,
        },
        DecisionReason::BetterDomain,
        enforce,
    )
}

const fn class_preference(mode: PlacementMode) -> ProcessorClassPreference {
    match mode {
        PlacementMode::Performance => ProcessorClassPreference::Fastest,
        PlacementMode::Efficiency => ProcessorClassPreference::MostEfficient,
        PlacementMode::Off
        | PlacementMode::Sticky
        | PlacementMode::Auto
        | PlacementMode::Strict(_) => ProcessorClassPreference::Any,
    }
}

fn least_loaded_domain(
    topology: &Topology,
    loads: &BTreeMap<LlcDomainKey, u16>,
    exclude: Option<LlcDomainKey>,
    class: ProcessorClassPreference,
) -> Result<(LlcDomainKey, Vec<u32>), AdaptiveError> {
    let mut best: Option<(&LlcDomain, Vec<u32>, u16)> = None;
    for domain in &topology.llc_domains {
        if Some(domain.key) == exclude {
            continue;
        }
        let ids = domain.cpu_set_ids_for_class(class);
        if ids.is_empty() {
            continue;
        }
        let load = loads[&domain.key];
        if best.as_ref().is_none_or(|(best_domain, _, best_load)| {
            (load, domain.key) < (*best_load, best_domain.key)
        }) {
            best = Some((domain, ids, load));
        }
    }
    best.map(|(domain, ids, _)| (domain.key, ids))
        .ok_or_else(|| {
            let key = exclude.unwrap_or(LlcDomainKey {
                group: 0,
                last_level_cache_index: 0,
            });
            AdaptiveError::NoAssignableCpuSets(key.group, key.last_level_cache_index)
        })
}

fn domain_cpu_set_ids(
    topology: &Topology,
    key: LlcDomainKey,
    class: ProcessorClassPreference,
) -> Result<Vec<u32>, AdaptiveError> {
    let Some(domain) = topology.llc_domains.iter().find(|domain| domain.key == key) else {
        return Err(AdaptiveError::UnknownProcessDomain(
            key.group,
            key.last_level_cache_index,
        ));
    };
    let ids = domain.cpu_set_ids_for_class(class);
    if ids.is_empty() {
        return Err(AdaptiveError::NoAssignableCpuSets(
            key.group,
            key.last_level_cache_index,
        ));
    }
    Ok(ids)
}

fn decision(
    process: &ProcessObservation,
    action: PolicyAction,
    reason: DecisionReason,
    enforce: bool,
) -> PolicyDecision {
    PolicyDecision {
        process: process.key,
        action,
        reason,
        enforce,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CpuSet, CpuSetFlags};

    const D0: LlcDomainKey = LlcDomainKey {
        group: 0,
        last_level_cache_index: 0,
    };
    const D1: LlcDomainKey = LlcDomainKey {
        group: 0,
        last_level_cache_index: 1,
    };

    fn cpu(id: u32, llc: u8, efficiency: u8) -> CpuSet {
        CpuSet {
            id,
            group: 0,
            logical_processor_index: u8::try_from(id).unwrap(),
            core_index: u8::try_from(id).unwrap(),
            last_level_cache_index: llc,
            numa_node_index: 0,
            efficiency_class: efficiency,
            scheduling_class: 0,
            flags: CpuSetFlags::default(),
            allocation_tag: 0,
        }
    }

    fn topology() -> Topology {
        Topology::new(vec![cpu(0, 0, 0), cpu(1, 0, 2), cpu(2, 1, 0), cpu(3, 1, 2)]).unwrap()
    }

    fn loads(d0: u16, d1: u16) -> Vec<DomainLoad> {
        vec![
            DomainLoad {
                domain: D0,
                utilization_bps: d0,
                dpc_time_bps: 0,
                interrupt_time_bps: 0,
            },
            DomainLoad {
                domain: D1,
                utilization_bps: d1,
                dpc_time_bps: 0,
                interrupt_time_bps: 0,
            },
        ]
    }

    fn process(mode: PlacementMode, current: Option<LlcDomainKey>) -> ProcessObservation {
        ProcessObservation {
            key: ProcessKey {
                pid: 42,
                creation_time_100ns: 7,
            },
            mode,
            enforcement: EnforcementMode::Apply,
            current_domain: current,
            assignment_origin: AssignmentOrigin::Managed,
            refresh_required: false,
            preferred_partition: None,
            exclusion: None,
        }
    }

    #[test]
    fn initial_auto_assignment_chooses_least_loaded_domain() {
        let mut engine = PolicyEngine::new(PolicyConfig::default()).unwrap();
        let decisions = engine
            .evaluate(
                0,
                &topology(),
                &loads(7_000, 1_000),
                &[process(PlacementMode::Auto, None)],
            )
            .unwrap();

        assert!(matches!(
            decisions[0].action,
            PolicyAction::Assign { target: D1, .. }
        ));
        assert!(decisions[0].enforce);
    }

    #[test]
    fn reserve_change_refreshes_the_current_domain_without_waiting_for_overload() {
        let mut engine = PolicyEngine::new(PolicyConfig::default()).unwrap();
        let mut observed = process(PlacementMode::Sticky, Some(D0));
        observed.refresh_required = true;

        let decisions = engine
            .evaluate(1_000, &topology(), &loads(1_000, 9_000), &[observed])
            .unwrap();

        assert_eq!(
            decisions[0].action,
            PolicyAction::Assign {
                target: D0,
                cpu_set_ids: vec![0, 1],
            }
        );
        assert_eq!(decisions[0].reason, DecisionReason::PartitionRefresh);
        assert!(decisions[0].enforce);
    }

    #[test]
    fn workload_profile_can_apply_and_keep_a_multi_llc_partition() {
        let mut engine = PolicyEngine::new(PolicyConfig::default()).unwrap();
        let partition = CpuPartition {
            anchor_domain: D0,
            physical_cores: Vec::new(),
            cpu_set_ids: vec![0, 2],
            llc_domains: vec![D0, D1],
            numa_nodes: vec![0],
            uses_smt: false,
        };
        let mut observed = process(PlacementMode::Sticky, None);
        observed.assignment_origin = AssignmentOrigin::None;
        observed.preferred_partition = Some(partition.clone());

        let decisions = engine
            .evaluate(0, &topology(), &loads(1_000, 1_000), &[observed.clone()])
            .unwrap();
        assert_eq!(
            decisions[0].action,
            PolicyAction::Assign {
                target: D0,
                cpu_set_ids: vec![0, 2],
            }
        );
        assert_eq!(decisions[0].reason, DecisionReason::ProfilePartition);

        assert!(engine.acknowledge(observed.key, Some(D0), true, 0));
        observed.current_domain = Some(D0);
        observed.assignment_origin = AssignmentOrigin::Managed;
        let decisions = engine
            .evaluate(1_000, &topology(), &loads(9_000, 100), &[observed])
            .unwrap();
        assert_eq!(decisions[0].action, PolicyAction::Keep { domain: Some(D0) });
        assert_eq!(decisions[0].reason, DecisionReason::ProfilePartitionStable);
    }

    #[test]
    fn unavailable_domains_degrade_to_noop_without_stopping_the_controller() {
        let mut unavailable = topology();
        for cpu in &mut unavailable.cpu_sets {
            cpu.flags.allocated = true;
        }
        unavailable = Topology::new(unavailable.cpu_sets).unwrap();
        let mut engine = PolicyEngine::new(PolicyConfig::default()).unwrap();
        let unassigned = process(PlacementMode::Auto, None);

        let decisions = engine
            .evaluate(0, &unavailable, &loads(1_000, 1_000), &[unassigned])
            .unwrap();
        assert_eq!(decisions[0].action, PolicyAction::Ignore);
        assert_eq!(decisions[0].reason, DecisionReason::NoAlternativeDomain);

        let strict = process(PlacementMode::Strict(D0), Some(D1));
        let decisions = engine
            .evaluate(1_000, &unavailable, &loads(1_000, 1_000), &[strict])
            .unwrap();
        assert_eq!(decisions[0].action, PolicyAction::Keep { domain: Some(D1) });
        assert_eq!(decisions[0].reason, DecisionReason::NoAlternativeDomain);
    }

    #[test]
    fn observe_reports_but_does_not_enforce() {
        let mut observed = process(PlacementMode::Auto, None);
        observed.enforcement = EnforcementMode::Observe;
        let mut engine = PolicyEngine::new(PolicyConfig::default()).unwrap();
        let decisions = engine
            .evaluate(0, &topology(), &loads(7_000, 1_000), &[observed])
            .unwrap();

        assert!(matches!(decisions[0].action, PolicyAction::Assign { .. }));
        assert!(!decisions[0].enforce);
    }

    #[test]
    fn observe_keeps_a_virtual_assignment_across_samples() {
        let mut observed = process(PlacementMode::Auto, None);
        observed.enforcement = EnforcementMode::Observe;
        let mut engine = PolicyEngine::new(PolicyConfig::default()).unwrap();
        let first = engine
            .evaluate(
                0,
                &topology(),
                &loads(1_000, 5_000),
                std::slice::from_ref(&observed),
            )
            .unwrap();
        assert!(matches!(
            first[0].action,
            PolicyAction::Assign { target: D0, .. }
        ));

        let second = engine
            .evaluate(1_000, &topology(), &loads(1_000, 5_000), &[observed])
            .unwrap();
        assert!(matches!(
            second[0].action,
            PolicyAction::Keep { domain: Some(D0) }
        ));
        assert_eq!(second[0].reason, DecisionReason::BelowOverloadThreshold);
    }

    #[test]
    fn external_assignment_is_not_overridden() {
        let mut observed = process(PlacementMode::Auto, Some(D0));
        observed.assignment_origin = AssignmentOrigin::External;
        let mut engine = PolicyEngine::new(PolicyConfig::default()).unwrap();
        let decisions = engine
            .evaluate(0, &topology(), &loads(9_000, 0), &[observed])
            .unwrap();

        assert_eq!(decisions[0].reason, DecisionReason::ExternalAssignment);
        assert_eq!(decisions[0].action, PolicyAction::Ignore);
    }

    #[test]
    fn sticky_mode_never_moves_an_existing_assignment() {
        let mut engine = PolicyEngine::new(PolicyConfig::default()).unwrap();
        let decisions = engine
            .evaluate(
                60_000,
                &topology(),
                &loads(10_000, 0),
                &[process(PlacementMode::Sticky, Some(D0))],
            )
            .unwrap();

        assert_eq!(decisions[0].reason, DecisionReason::StickyPlacement);
        assert!(matches!(
            decisions[0].action,
            PolicyAction::Keep { domain: Some(D0) }
        ));
    }

    #[test]
    fn auto_requires_stable_overload_before_move() {
        let config = PolicyConfig {
            minimum_residency_ms: 0,
            cooldown_ms: 0,
            ..PolicyConfig::default()
        };
        let mut engine = PolicyEngine::new(config).unwrap();
        for sample in 0u64..2 {
            let decisions = engine
                .evaluate(
                    sample * 1_000,
                    &topology(),
                    &loads(9_000, 1_000),
                    &[process(PlacementMode::Auto, Some(D0))],
                )
                .unwrap();
            assert_eq!(decisions[0].reason, DecisionReason::StabilityWindow);
        }

        let decisions = engine
            .evaluate(
                2_000,
                &topology(),
                &loads(9_000, 1_000),
                &[process(PlacementMode::Auto, Some(D0))],
            )
            .unwrap();
        assert!(matches!(
            decisions[0].action,
            PolicyAction::Move {
                source: D0,
                target: D1,
                ..
            }
        ));
    }

    #[test]
    fn insufficient_improvement_prevents_move() {
        let config = PolicyConfig {
            stability_samples: 1,
            minimum_residency_ms: 0,
            cooldown_ms: 0,
            ..PolicyConfig::default()
        };
        let mut engine = PolicyEngine::new(config).unwrap();
        let decisions = engine
            .evaluate(
                1,
                &topology(),
                &loads(9_000, 7_500),
                &[process(PlacementMode::Auto, Some(D0))],
            )
            .unwrap();

        assert_eq!(decisions[0].reason, DecisionReason::InsufficientImprovement);
    }

    #[test]
    fn minimum_residency_and_cooldown_are_enforced() {
        let config = PolicyConfig {
            stability_samples: 1,
            minimum_residency_ms: 10_000,
            cooldown_ms: 30_000,
            ..PolicyConfig::default()
        };
        let mut engine = PolicyEngine::new(config).unwrap();
        let first = engine
            .evaluate(
                1_000,
                &topology(),
                &loads(9_000, 0),
                &[process(PlacementMode::Auto, Some(D0))],
            )
            .unwrap();
        assert_eq!(first[0].reason, DecisionReason::MinimumResidency);

        let third = engine
            .evaluate(
                11_000,
                &topology(),
                &loads(9_000, 0),
                &[process(PlacementMode::Auto, Some(D0))],
            )
            .unwrap();
        assert!(matches!(
            third[0].action,
            PolicyAction::Move { target: D1, .. }
        ));
        assert!(engine.acknowledge(
            process(PlacementMode::Auto, Some(D0)).key,
            Some(D1),
            true,
            11_000
        ));

        let after_move = process(PlacementMode::Auto, Some(D1));
        let cooldown = engine
            .evaluate(22_000, &topology(), &loads(0, 9_000), &[after_move])
            .unwrap();
        assert_eq!(cooldown[0].reason, DecisionReason::Cooldown);
    }

    #[test]
    fn failed_mutation_does_not_advance_assignment() {
        let mut engine = PolicyEngine::new(PolicyConfig::default()).unwrap();
        let observed = process(PlacementMode::Auto, None);
        let decisions = engine
            .evaluate(
                0,
                &topology(),
                &loads(8_000, 0),
                std::slice::from_ref(&observed),
            )
            .unwrap();
        let PolicyAction::Assign { target, .. } = decisions[0].action else {
            panic!("expected assignment");
        };
        assert!(engine.acknowledge(observed.key, Some(target), false, 1));

        let retry = engine
            .evaluate(2, &topology(), &loads(8_000, 0), &[observed])
            .unwrap();
        assert!(matches!(retry[0].action, PolicyAction::Assign { .. }));
    }

    #[test]
    fn strict_mode_overrides_external_assignment() {
        let mut observed = process(PlacementMode::Strict(D1), Some(D0));
        observed.assignment_origin = AssignmentOrigin::External;
        let mut engine = PolicyEngine::new(PolicyConfig::default()).unwrap();
        let decisions = engine
            .evaluate(0, &topology(), &loads(0, 9_000), &[observed])
            .unwrap();

        assert_eq!(decisions[0].reason, DecisionReason::StrictPlacement);
        assert!(matches!(
            decisions[0].action,
            PolicyAction::Move { target: D1, .. }
        ));
    }

    #[test]
    fn off_clears_only_managed_assignment() {
        let mut engine = PolicyEngine::new(PolicyConfig::default()).unwrap();
        let decisions = engine
            .evaluate(
                0,
                &topology(),
                &loads(0, 0),
                &[process(PlacementMode::Off, Some(D0))],
            )
            .unwrap();
        assert!(matches!(
            decisions[0].action,
            PolicyAction::Clear { source: D0 }
        ));
    }

    #[test]
    fn performance_and_efficiency_filter_classes() {
        let mut performance = PolicyEngine::new(PolicyConfig::default()).unwrap();
        let fast = performance
            .evaluate(
                0,
                &topology(),
                &loads(0, 5_000),
                &[process(PlacementMode::Performance, None)],
            )
            .unwrap();
        let PolicyAction::Assign { cpu_set_ids, .. } = &fast[0].action else {
            panic!("expected assignment");
        };
        assert_eq!(cpu_set_ids, &[1]);

        let mut efficiency = PolicyEngine::new(PolicyConfig::default()).unwrap();
        let efficient = efficiency
            .evaluate(
                0,
                &topology(),
                &loads(0, 5_000),
                &[process(PlacementMode::Efficiency, None)],
            )
            .unwrap();
        let PolicyAction::Assign { cpu_set_ids, .. } = &efficient[0].action else {
            panic!("expected assignment");
        };
        assert_eq!(cpu_set_ids, &[0]);
    }

    #[test]
    fn mutation_budget_rate_limits_second_process() {
        let mut second = process(PlacementMode::Auto, None);
        second.key.pid = 43;
        second.key.creation_time_100ns = 8;
        let mut engine = PolicyEngine::new(PolicyConfig::default()).unwrap();
        let decisions = engine
            .evaluate(
                0,
                &topology(),
                &loads(0, 0),
                &[process(PlacementMode::Auto, None), second],
            )
            .unwrap();

        assert!(decisions[0].action.is_mutation());
        assert_eq!(decisions[1].reason, DecisionReason::RateLimited);
    }

    #[test]
    fn observe_suggestions_are_not_mutation_rate_limited() {
        let mut first = process(PlacementMode::Auto, None);
        first.enforcement = EnforcementMode::Observe;
        let mut second = process(PlacementMode::Auto, None);
        second.enforcement = EnforcementMode::Observe;
        second.key.pid = 43;
        second.key.creation_time_100ns = 8;
        let mut engine = PolicyEngine::new(PolicyConfig::default()).unwrap();
        let decisions = engine
            .evaluate(0, &topology(), &loads(0, 0), &[first, second])
            .unwrap();

        assert!(decisions.iter().all(|decision| {
            decision.action.is_mutation()
                && !decision.enforce
                && decision.reason == DecisionReason::InitialPlacement
        }));
    }

    #[test]
    fn missing_domain_load_is_rejected() {
        let mut engine = PolicyEngine::new(PolicyConfig::default()).unwrap();
        let error = engine
            .evaluate(
                0,
                &topology(),
                &[DomainLoad {
                    domain: D0,
                    utilization_bps: 0,
                    dpc_time_bps: 0,
                    interrupt_time_bps: 0,
                }],
                &[],
            )
            .unwrap_err();
        assert_eq!(error, AdaptiveError::MissingDomainLoad(0, 1));
    }
}
