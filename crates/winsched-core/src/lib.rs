//! Platform-independent CPU topology and placement policy.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod adaptive;

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
}
