//! Platform-independent CPU topology and placement policy.

#![forbid(unsafe_code)]

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod adaptive;
pub mod latency;
pub mod responsiveness;

/// A stable CPU Set snapshot returned by the Windows platform layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuSet {
    pub id: u32,
    pub group: u16,
    pub logical_processor_index: u8,
    pub core_index: u8,
    pub last_level_cache_index: u8,
    pub numa_node_index: u8,
    pub efficiency_class: u8,
    pub scheduling_class: u8,
    pub flags: CpuSetFlags,
    pub allocation_tag: u64,
}

/// State flags reported for one Windows CPU Set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // Mirrors the four independent Win32 bit flags.
pub struct CpuSetFlags {
    pub parked: bool,
    pub allocated: bool,
    pub allocated_to_target_process: bool,
    pub realtime: bool,
}

impl CpuSet {
    /// Returns whether this CPU Set may safely be selected for the target.
    #[must_use]
    pub const fn is_assignable(&self) -> bool {
        !self.flags.parked
            && !self.flags.realtime
            && (!self.flags.allocated || self.flags.allocated_to_target_process)
    }
}

/// A group-relative LLC identity, matching Windows CPU Set semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LlcDomainKey {
    pub group: u16,
    pub last_level_cache_index: u8,
}

/// A physical core identity shared by all of its SMT CPU Sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PhysicalCoreKey {
    pub group: u16,
    pub core_index: u8,
}

/// Topology and scheduler ranking for one complete physical core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalCore {
    pub key: PhysicalCoreKey,
    pub llc_domain: LlcDomainKey,
    pub numa_node_index: u8,
    pub cpu_set_ids: Vec<u32>,
    pub maximum_scheduling_class: u8,
}

/// CPU Sets held back from managed application assignments for OS responsiveness.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemReservePlan {
    pub physical_core_count: usize,
    pub requested_core_count: usize,
    pub reserved_physical_cores: Vec<PhysicalCoreKey>,
    pub reserved_cpu_set_ids: Vec<u32>,
    pub covered_llc_domains: Vec<LlcDomainKey>,
}

/// A stable CPU Set partition that may span multiple LLC domains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuPartition {
    pub anchor_domain: LlcDomainKey,
    pub physical_cores: Vec<PhysicalCoreKey>,
    pub cpu_set_ids: Vec<u32>,
    pub llc_domains: Vec<LlcDomainKey>,
    pub numa_nodes: Vec<u8>,
    pub uses_smt: bool,
}

/// CPU Sets that share a Last Level Cache within one processor group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlcDomain {
    pub key: LlcDomainKey,
    pub cpu_sets: Vec<CpuSet>,
    pub numa_nodes: Vec<u8>,
    pub core_indices: Vec<u8>,
    pub efficiency_classes: Vec<u8>,
}

impl LlcDomain {
    /// Returns assignable CPU Set IDs matching one processor-class preference.
    #[must_use]
    pub fn cpu_set_ids_for_class(&self, preference: ProcessorClassPreference) -> Vec<u32> {
        let selected_class = match preference {
            ProcessorClassPreference::Any => None,
            ProcessorClassPreference::Fastest => self
                .cpu_sets
                .iter()
                .filter(|cpu| cpu.is_assignable())
                .map(|cpu| cpu.efficiency_class)
                .max(),
            ProcessorClassPreference::MostEfficient => self
                .cpu_sets
                .iter()
                .filter(|cpu| cpu.is_assignable())
                .map(|cpu| cpu.efficiency_class)
                .min(),
        };

        self.cpu_sets
            .iter()
            .filter(|cpu| cpu.is_assignable())
            .filter(|cpu| {
                preference == ProcessorClassPreference::Any
                    || Some(cpu.efficiency_class) == selected_class
            })
            .map(|cpu| cpu.id)
            .collect()
    }

    #[must_use]
    pub fn assignable_cpu_set_ids(&self, performance_only: bool) -> Vec<u32> {
        self.cpu_set_ids_for_class(if performance_only {
            ProcessorClassPreference::Fastest
        } else {
            ProcessorClassPreference::Any
        })
    }

    #[must_use]
    pub fn assignable_count(&self, performance_only: bool) -> usize {
        self.assignable_cpu_set_ids(performance_only).len()
    }

    #[must_use]
    pub fn maximum_efficiency_class(&self) -> Option<u8> {
        self.cpu_sets
            .iter()
            .filter(|cpu| cpu.is_assignable())
            .map(|cpu| cpu.efficiency_class)
            .max()
    }
}

/// How a policy filters heterogeneous processor classes inside one LLC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessorClassPreference {
    Any,
    Fastest,
    MostEfficient,
}

/// A validated topology snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Topology {
    pub cpu_sets: Vec<CpuSet>,
    pub llc_domains: Vec<LlcDomain>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TopologyError {
    #[error("the topology contains duplicate CPU Set ID {0}")]
    DuplicateCpuSetId(u32),
    #[error("the topology contains no CPU Sets")]
    Empty,
}

impl Topology {
    /// Builds deterministic LLC domains from a CPU Set snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`TopologyError::Empty`] when no CPU Sets are provided, or
    /// [`TopologyError::DuplicateCpuSetId`] when IDs are not unique.
    pub fn new(mut cpu_sets: Vec<CpuSet>) -> Result<Self, TopologyError> {
        if cpu_sets.is_empty() {
            return Err(TopologyError::Empty);
        }

        let mut ids = BTreeSet::new();
        for cpu in &cpu_sets {
            if !ids.insert(cpu.id) {
                return Err(TopologyError::DuplicateCpuSetId(cpu.id));
            }
        }
        cpu_sets.sort_by_key(|cpu| (cpu.group, cpu.logical_processor_index, cpu.id));

        let mut grouped = BTreeMap::<LlcDomainKey, Vec<CpuSet>>::new();
        for cpu in cpu_sets.iter().cloned() {
            grouped
                .entry(LlcDomainKey {
                    group: cpu.group,
                    last_level_cache_index: cpu.last_level_cache_index,
                })
                .or_default()
                .push(cpu);
        }

        let llc_domains = grouped
            .into_iter()
            .map(|(key, domain_cpu_sets)| {
                let numa_nodes =
                    unique_sorted(domain_cpu_sets.iter().map(|cpu| cpu.numa_node_index));
                let core_indices = unique_sorted(domain_cpu_sets.iter().map(|cpu| cpu.core_index));
                let efficiency_classes =
                    unique_sorted(domain_cpu_sets.iter().map(|cpu| cpu.efficiency_class));
                LlcDomain {
                    key,
                    cpu_sets: domain_cpu_sets,
                    numa_nodes,
                    core_indices,
                    efficiency_classes,
                }
            })
            .collect();

        Ok(Self {
            cpu_sets,
            llc_domains,
        })
    }

    /// Resolves a non-empty CPU Set selection when every ID belongs to one LLC.
    #[must_use]
    pub fn domain_for_cpu_set_ids(&self, cpu_set_ids: &[u32]) -> Option<LlcDomainKey> {
        let mut resolved = None;
        for id in cpu_set_ids {
            let cpu = self.cpu_sets.iter().find(|cpu| cpu.id == *id)?;
            let key = LlcDomainKey {
                group: cpu.group,
                last_level_cache_index: cpu.last_level_cache_index,
            };
            if resolved.is_some_and(|existing| existing != key) {
                return None;
            }
            resolved = Some(key);
        }
        resolved
    }

    /// Groups CPU Sets into physical cores and rejects ambiguous cross-locality siblings.
    #[must_use]
    pub fn physical_cores(&self) -> Vec<PhysicalCore> {
        let mut grouped = BTreeMap::<PhysicalCoreKey, Vec<&CpuSet>>::new();
        for cpu in &self.cpu_sets {
            grouped
                .entry(PhysicalCoreKey {
                    group: cpu.group,
                    core_index: cpu.core_index,
                })
                .or_default()
                .push(cpu);
        }

        grouped
            .into_iter()
            .filter_map(|(key, mut siblings)| {
                siblings.sort_by_key(|cpu| (cpu.logical_processor_index, cpu.id));
                let first = siblings.first()?;
                let llc_domain = LlcDomainKey {
                    group: first.group,
                    last_level_cache_index: first.last_level_cache_index,
                };
                if siblings.iter().any(|cpu| {
                    cpu.group != key.group
                        || cpu.last_level_cache_index != llc_domain.last_level_cache_index
                        || cpu.numa_node_index != first.numa_node_index
                }) {
                    return None;
                }
                Some(PhysicalCore {
                    key,
                    llc_domain,
                    numa_node_index: first.numa_node_index,
                    cpu_set_ids: siblings.iter().map(|cpu| cpu.id).collect(),
                    maximum_scheduling_class: siblings
                        .iter()
                        .map(|cpu| cpu.scheduling_class)
                        .max()
                        .unwrap_or(0),
                })
            })
            .collect()
    }

    /// Builds a deterministic whole-core reserve spread evenly over LLC domains.
    #[must_use]
    pub fn plan_system_reserve(
        &self,
        percent: u8,
        minimum_cores: u16,
        maximum_cores: u16,
    ) -> SystemReservePlan {
        let physical_cores = self.physical_cores();
        let physical_core_count = physical_cores.len();
        let requested_core_count =
            reserve_core_target(physical_core_count, percent, minimum_cores, maximum_cores);

        let selected = self.select_spread_physical_cores(requested_core_count);
        let reserved_physical_cores = selected.iter().map(|core| core.key).collect::<Vec<_>>();
        let mut reserved_cpu_set_ids = selected
            .iter()
            .flat_map(|core| core.cpu_set_ids.iter().copied())
            .collect::<Vec<_>>();
        reserved_cpu_set_ids.sort_unstable();
        reserved_cpu_set_ids.dedup();
        let covered_llc_domains = selected
            .iter()
            .map(|core| core.llc_domain)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        SystemReservePlan {
            physical_core_count,
            requested_core_count,
            reserved_physical_cores,
            reserved_cpu_set_ids,
            covered_llc_domains,
        }
    }

    /// Builds a deterministic multi-LLC partition from the best available whole cores.
    #[must_use]
    pub fn plan_spread_partition(
        &self,
        maximum_physical_cores: usize,
        use_smt: bool,
    ) -> Option<CpuPartition> {
        let selected = self.select_spread_physical_cores(maximum_physical_cores);
        let anchor_domain = selected.first()?.llc_domain;
        let physical_cores = selected.iter().map(|core| core.key).collect::<Vec<_>>();
        let mut cpu_set_ids = Vec::new();
        for core in &selected {
            let mut siblings = core
                .cpu_set_ids
                .iter()
                .filter_map(|id| self.cpu_sets.iter().find(|cpu| cpu.id == *id))
                .filter(|cpu| cpu.is_assignable())
                .collect::<Vec<_>>();
            siblings.sort_by_key(|cpu| {
                (
                    Reverse(cpu.scheduling_class),
                    cpu.logical_processor_index,
                    cpu.id,
                )
            });
            if use_smt {
                cpu_set_ids.extend(siblings.into_iter().map(|cpu| cpu.id));
            } else if let Some(cpu) = siblings.first() {
                cpu_set_ids.push(cpu.id);
            }
        }
        cpu_set_ids.sort_unstable();
        cpu_set_ids.dedup();
        if cpu_set_ids.is_empty() {
            return None;
        }
        let llc_domains = selected
            .iter()
            .map(|core| core.llc_domain)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let numa_nodes = selected
            .iter()
            .map(|core| core.numa_node_index)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Some(CpuPartition {
            anchor_domain,
            physical_cores,
            cpu_set_ids,
            llc_domains,
            numa_nodes,
            uses_smt: use_smt,
        })
    }

    /// Counts physical cores with at least one assignable CPU Set.
    #[must_use]
    pub fn assignable_physical_core_count(&self) -> usize {
        self.select_spread_physical_cores(usize::MAX).len()
    }

    /// Returns a placement view in which reserved CPU Sets are unavailable to policies.
    ///
    /// # Panics
    ///
    /// Panics only if cloning a previously validated topology introduces a duplicate CPU Set ID,
    /// which cannot occur without an internal invariant violation.
    #[must_use]
    pub fn excluding_reserved_cpu_sets(&self, reserve: &SystemReservePlan) -> Self {
        let reserved = reserve
            .reserved_cpu_set_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let mut cpu_sets = self.cpu_sets.clone();
        for cpu in &mut cpu_sets {
            if reserved.contains(&cpu.id) {
                cpu.flags.allocated = true;
                cpu.flags.allocated_to_target_process = false;
            }
        }
        Self::new(cpu_sets).expect("reserving CPU Sets preserves topology validity")
    }

    fn select_spread_physical_cores(&self, requested: usize) -> Vec<PhysicalCore> {
        let mut domains = BTreeMap::<LlcDomainKey, Vec<PhysicalCore>>::new();
        for core in self.physical_cores().into_iter().filter(|core| {
            let siblings = core
                .cpu_set_ids
                .iter()
                .filter_map(|id| self.cpu_sets.iter().find(|cpu| cpu.id == *id))
                .collect::<Vec<_>>();
            siblings.iter().any(|cpu| cpu.is_assignable())
                && siblings.iter().all(|cpu| {
                    !cpu.flags.realtime
                        && (!cpu.flags.allocated || cpu.flags.allocated_to_target_process)
                })
        }) {
            domains.entry(core.llc_domain).or_default().push(core);
        }
        for cores in domains.values_mut() {
            cores.sort_by_key(|core| (Reverse(core.maximum_scheduling_class), core.key));
        }

        let available = domains.values().map(Vec::len).sum::<usize>();
        let target = requested.min(available);
        let domain_keys = domains.keys().copied().collect::<Vec<_>>();
        let domain_order = spread_indices(domain_keys.len(), target);
        let mut selected = Vec::new();
        let mut depths = vec![0usize; domain_keys.len()];
        for domain_index in domain_order {
            let Some(cores) = domains.get(&domain_keys[domain_index]) else {
                continue;
            };
            let depth = depths[domain_index];
            if let Some(core) = cores.get(depth) {
                selected.push(core.clone());
                depths[domain_index] = depth + 1;
            }
        }

        if selected.len() < target {
            'fill: loop {
                let mut changed = false;
                for (domain_index, key) in domain_keys.iter().enumerate() {
                    if selected.len() == target {
                        break 'fill;
                    }
                    let Some(cores) = domains.get(key) else {
                        continue;
                    };
                    let depth = depths[domain_index];
                    if let Some(core) = cores.get(depth) {
                        selected.push(core.clone());
                        depths[domain_index] = depth + 1;
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }
        }

        selected.sort_by_key(|core| core.key);
        selected
    }
}

fn reserve_core_target(total: usize, percent: u8, minimum: u16, maximum: u16) -> usize {
    if total <= 1 || percent == 0 || maximum == 0 {
        return 0;
    }
    let percentage = total.saturating_mul(usize::from(percent)).div_ceil(100);
    percentage
        .max(usize::from(minimum))
        .min(usize::from(maximum))
        .min(total - 1)
}

fn spread_indices(domain_count: usize, requested: usize) -> Vec<usize> {
    if domain_count == 0 || requested == 0 {
        return Vec::new();
    }
    let first_pass = requested.min(domain_count);
    (0..first_pass)
        .map(|index| index.saturating_mul(domain_count) / first_pass)
        .collect()
}

fn unique_sorted<T: Ord>(values: impl Iterator<Item = T>) -> Vec<T> {
    values.collect::<BTreeSet<_>>().into_iter().collect()
}

/// A requested LLC selection policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainSelector {
    Auto,
    Exact(LlcDomainKey),
}

/// A side-effect-free CPU Set assignment decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssignmentPlan {
    pub domain: LlcDomainKey,
    pub cpu_set_ids: Vec<u32>,
    pub performance_only: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("LLC domain group={group}, llc={llc} does not exist")]
    DomainNotFound { group: u16, llc: u8 },
    #[error("no assignable CPU Sets satisfy the request")]
    NoAssignableCpuSets,
}

/// Produces a deterministic assignment without touching the operating system.
///
/// # Errors
///
/// Returns [`PolicyError::DomainNotFound`] for an unknown explicit domain, or
/// [`PolicyError::NoAssignableCpuSets`] when all matching sets are unavailable.
pub fn plan_assignment(
    topology: &Topology,
    selector: DomainSelector,
    performance_only: bool,
) -> Result<AssignmentPlan, PolicyError> {
    let domain = match selector {
        DomainSelector::Exact(key) => topology
            .llc_domains
            .iter()
            .find(|domain| domain.key == key)
            .ok_or(PolicyError::DomainNotFound {
                group: key.group,
                llc: key.last_level_cache_index,
            })?,
        DomainSelector::Auto => {
            let mut best = None;
            for candidate in &topology.llc_domains {
                let score = (
                    candidate.assignable_count(performance_only),
                    candidate.maximum_efficiency_class().unwrap_or(0),
                );
                if best.is_none_or(|(_, best_score)| score > best_score) {
                    best = Some((candidate, score));
                }
            }
            best.map(|(domain, _)| domain)
                .ok_or(PolicyError::NoAssignableCpuSets)?
        }
    };

    let cpu_set_ids = domain.assignable_cpu_set_ids(performance_only);
    if cpu_set_ids.is_empty() {
        return Err(PolicyError::NoAssignableCpuSets);
    }

    Ok(AssignmentPlan {
        domain: domain.key,
        cpu_set_ids,
        performance_only,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu(id: u32, group: u16, llc: u8, logical: u8, efficiency: u8) -> CpuSet {
        CpuSet {
            id,
            group,
            logical_processor_index: logical,
            core_index: logical / 2,
            last_level_cache_index: llc,
            numa_node_index: 0,
            efficiency_class: efficiency,
            scheduling_class: 0,
            flags: CpuSetFlags::default(),
            allocation_tag: 0,
        }
    }

    fn threadripper_3970x_topology() -> Topology {
        let mut cpu_sets = Vec::new();
        for domain in 0u8..8 {
            for core_in_domain in 0u8..4 {
                let core_index = domain * 8 + core_in_domain * 2;
                for sibling in 0u8..2 {
                    let logical = core_index + sibling;
                    cpu_sets.push(CpuSet {
                        id: 256 + u32::from(logical),
                        group: 0,
                        logical_processor_index: logical,
                        core_index,
                        last_level_cache_index: domain * 8,
                        numa_node_index: 0,
                        efficiency_class: 0,
                        scheduling_class: domain * 4 + core_in_domain,
                        flags: CpuSetFlags::default(),
                        allocation_tag: 0,
                    });
                }
            }
        }
        Topology::new(cpu_sets).unwrap()
    }

    #[test]
    fn groups_cpu_sets_by_group_relative_llc() {
        let topology = Topology::new(vec![
            cpu(3, 1, 0, 1, 2),
            cpu(1, 0, 0, 1, 1),
            cpu(2, 1, 0, 0, 2),
            cpu(0, 0, 0, 0, 1),
        ])
        .unwrap();

        assert_eq!(topology.llc_domains.len(), 2);
        assert_eq!(topology.llc_domains[0].key.group, 0);
        assert_eq!(topology.llc_domains[1].key.group, 1);
    }

    #[test]
    fn rejects_duplicate_cpu_set_ids() {
        let error = Topology::new(vec![
            cpu(7, 0, 0, 0, 0),
            cpu(8, 0, 1, 1, 0),
            cpu(7, 1, 1, 2, 0),
        ])
        .unwrap_err();
        assert_eq!(error, TopologyError::DuplicateCpuSetId(7));
    }

    #[test]
    fn auto_selects_domain_with_most_assignable_sets() {
        let topology = Topology::new(vec![
            cpu(0, 0, 0, 0, 0),
            cpu(1, 0, 1, 1, 0),
            cpu(2, 0, 1, 2, 0),
        ])
        .unwrap();

        let plan = plan_assignment(&topology, DomainSelector::Auto, false).unwrap();
        assert_eq!(plan.domain.last_level_cache_index, 1);
        assert_eq!(plan.cpu_set_ids, vec![1, 2]);
    }

    #[test]
    fn exact_selection_is_deterministic() {
        let topology = Topology::new(vec![cpu(0, 0, 0, 0, 0), cpu(1, 0, 1, 1, 0)]).unwrap();
        let key = LlcDomainKey {
            group: 0,
            last_level_cache_index: 0,
        };

        let plan = plan_assignment(&topology, DomainSelector::Exact(key), false).unwrap();
        assert_eq!(plan.domain, key);
        assert_eq!(plan.cpu_set_ids, vec![0]);
    }

    #[test]
    fn performance_filter_keeps_fastest_class() {
        let topology = Topology::new(vec![cpu(0, 0, 0, 0, 0), cpu(1, 0, 0, 1, 3)]).unwrap();

        let plan = plan_assignment(&topology, DomainSelector::Auto, true).unwrap();
        assert_eq!(plan.cpu_set_ids, vec![1]);
    }

    #[test]
    fn excludes_cpu_sets_allocated_to_another_process() {
        let mut unavailable = cpu(0, 0, 0, 0, 0);
        unavailable.flags.allocated = true;
        let topology = Topology::new(vec![unavailable, cpu(1, 0, 0, 1, 0)]).unwrap();

        let plan = plan_assignment(&topology, DomainSelector::Auto, false).unwrap();
        assert_eq!(plan.cpu_set_ids, vec![1]);
    }

    #[test]
    fn excludes_parked_cpu_sets_from_assignment() {
        let mut parked = cpu(0, 0, 0, 0, 0);
        parked.flags.parked = true;
        let topology = Topology::new(vec![parked, cpu(1, 0, 0, 1, 0)]).unwrap();

        let plan = plan_assignment(&topology, DomainSelector::Auto, false).unwrap();
        assert_eq!(plan.cpu_set_ids, vec![1]);
    }

    #[test]
    fn resolves_only_single_domain_cpu_set_selections() {
        let topology = Topology::new(vec![
            cpu(0, 0, 0, 0, 0),
            cpu(1, 0, 0, 1, 0),
            cpu(2, 0, 1, 2, 0),
        ])
        .unwrap();

        assert_eq!(
            topology.domain_for_cpu_set_ids(&[0, 1]),
            Some(LlcDomainKey {
                group: 0,
                last_level_cache_index: 0,
            })
        );
        assert_eq!(topology.domain_for_cpu_set_ids(&[0, 2]), None);
        assert_eq!(topology.domain_for_cpu_set_ids(&[]), None);
        assert_eq!(topology.domain_for_cpu_set_ids(&[99]), None);
    }

    #[test]
    fn threadripper_reserve_rounds_ten_percent_to_four_whole_cores() {
        let topology = threadripper_3970x_topology();
        let plan = topology.plan_system_reserve(10, 2, 8);

        assert_eq!(plan.physical_core_count, 32);
        assert_eq!(plan.requested_core_count, 4);
        assert_eq!(plan.reserved_physical_cores.len(), 4);
        assert_eq!(plan.reserved_cpu_set_ids.len(), 8);
        assert_eq!(
            plan.covered_llc_domains,
            [0, 16, 32, 48]
                .into_iter()
                .map(|last_level_cache_index| LlcDomainKey {
                    group: 0,
                    last_level_cache_index,
                })
                .collect::<Vec<_>>()
        );
        assert_eq!(
            plan.reserved_physical_cores,
            [6, 22, 38, 54]
                .into_iter()
                .map(|core_index| PhysicalCoreKey {
                    group: 0,
                    core_index,
                })
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn reserved_cpu_sets_are_unavailable_to_application_plans() {
        let topology = threadripper_3970x_topology();
        let reserve = topology.plan_system_reserve(10, 2, 8);
        let placement = topology.excluding_reserved_cpu_sets(&reserve);

        for key in &reserve.covered_llc_domains {
            let plan = plan_assignment(&placement, DomainSelector::Exact(*key), false).unwrap();
            assert_eq!(plan.cpu_set_ids.len(), 6);
            assert!(
                plan.cpu_set_ids
                    .iter()
                    .all(|id| !reserve.reserved_cpu_set_ids.contains(id))
            );
        }
    }

    #[test]
    fn memory_partition_spreads_one_thread_per_remaining_physical_core() {
        let topology = threadripper_3970x_topology();
        let reserve = topology.plan_system_reserve(10, 2, 8);
        let placement = topology.excluding_reserved_cpu_sets(&reserve);
        assert_eq!(placement.assignable_physical_core_count(), 28);
        let partition = placement.plan_spread_partition(28, false).unwrap();

        assert_eq!(partition.physical_cores.len(), 28);
        assert_eq!(partition.cpu_set_ids.len(), 28);
        assert_eq!(partition.llc_domains.len(), 8);
        assert_eq!(partition.numa_nodes, vec![0]);
        assert!(!partition.uses_smt);
        assert!(
            partition
                .cpu_set_ids
                .iter()
                .all(|id| !reserve.reserved_cpu_set_ids.contains(id))
        );
    }

    #[test]
    fn compute_partition_keeps_both_smt_siblings_outside_the_reserve() {
        let topology = threadripper_3970x_topology();
        let reserve = topology.plan_system_reserve(10, 2, 8);
        let placement = topology.excluding_reserved_cpu_sets(&reserve);
        let partition = placement.plan_spread_partition(usize::MAX, true).unwrap();

        assert_eq!(partition.physical_cores.len(), 28);
        assert_eq!(partition.cpu_set_ids.len(), 56);
        assert!(partition.uses_smt);
    }

    #[test]
    fn reserve_never_consumes_the_only_physical_core() {
        assert_eq!(reserve_core_target(1, 50, 1, 8), 0);
        assert_eq!(reserve_core_target(4, 10, 2, 8), 2);
        assert_eq!(reserve_core_target(32, 10, 2, 8), 4);
        assert_eq!(reserve_core_target(96, 10, 2, 8), 8);
    }
}
