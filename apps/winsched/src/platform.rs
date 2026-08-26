#![allow(clippy::missing_errors_doc)] // Thin facade preserves detailed PlatformError values.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use winsched_control::InteractiveActivityState;
use winsched_core::{
    LlcDomainKey, Topology,
    adaptive::{
        AssignmentOrigin, DomainLoad, EnforcementMode, ExclusionReason, PlacementMode, ProcessKey,
        ProcessObservation,
    },
};

#[cfg(any(windows, test))]
mod safety;
#[cfg(windows)]
mod windows;

#[derive(Debug, Error)]
pub enum PlatformError {
    #[cfg(windows)]
    #[error(transparent)]
    Windows(#[from] windows::PlatformError),
    #[cfg(not(windows))]
    #[error("this command requires Windows 11; build and run the x86_64-pc-windows-msvc target")]
    UnsupportedPlatform,
}

impl PlatformError {
    /// Returns true when the journal identity is known to have exited or been reused.
    #[must_use]
    pub fn process_no_longer_matches(&self) -> bool {
        #[cfg(windows)]
        {
            match self {
                Self::Windows(error) => error.process_no_longer_matches(),
            }
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// Returns true when an expected-state check detected another policy owner.
    #[must_use]
    pub fn efficiency_ownership_changed(&self) -> bool {
        #[cfg(windows)]
        {
            match self {
                Self::Windows(error) => error.efficiency_ownership_changed(),
            }
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessSnapshot {
    pub pid: u32,
    pub default_cpu_set_ids: Vec<u32>,
    pub topology: Topology,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessEfficiencySnapshot {
    pub key: ProcessKey,
    pub state: ProcessEfficiencyState,
}

#[derive(Debug, Clone, Serialize)]
pub struct MutationReport {
    pub operation: String,
    pub pid: u32,
    pub committed: bool,
    pub previous_cpu_set_ids: Vec<u32>,
    pub requested_cpu_set_ids: Vec<u32>,
    pub observed_cpu_set_ids: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessEcoQosState {
    Unset,
    Disabled,
    Enabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessMemoryPriority {
    VeryLow,
    Low,
    Medium,
    BelowNormal,
    Normal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessEfficiencyState {
    pub eco_qos: ProcessEcoQosState,
    pub memory_priority: ProcessMemoryPriority,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessEfficiencyOwnership {
    pub eco_qos: bool,
    pub memory_priority: bool,
}

impl ProcessEfficiencyOwnership {
    #[must_use]
    pub fn between(original: ProcessEfficiencyState, applied: ProcessEfficiencyState) -> Self {
        Self {
            eco_qos: original.eco_qos != applied.eco_qos,
            memory_priority: original.memory_priority != applied.memory_priority,
        }
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self {
            eco_qos: self.eco_qos || other.eco_qos,
            memory_priority: self.memory_priority || other.memory_priority,
        }
    }

    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self {
            eco_qos: self.eco_qos && other.eco_qos,
            memory_priority: self.memory_priority && other.memory_priority,
        }
    }

    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self {
            eco_qos: self.eco_qos && !other.eco_qos,
            memory_priority: self.memory_priority && !other.memory_priority,
        }
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        !self.eco_qos && !self.memory_priority
    }

    #[must_use]
    pub fn matches(
        self,
        expected: ProcessEfficiencyState,
        observed: ProcessEfficiencyState,
    ) -> bool {
        (!self.eco_qos || expected.eco_qos == observed.eco_qos)
            && (!self.memory_priority || expected.memory_priority == observed.memory_priority)
    }
}

#[derive(Debug, Clone, Serialize)]
#[allow(clippy::struct_excessive_bools)] // The report exposes independent rollback outcomes.
pub struct EfficiencyMutationReport {
    pub operation: String,
    pub pid: u32,
    pub committed: bool,
    pub previous: ProcessEfficiencyState,
    pub requested: ProcessEfficiencyState,
    pub observed: ProcessEfficiencyState,
    pub eco_qos_changed: bool,
    pub memory_priority_changed: bool,
    pub external_eco_qos_preserved: bool,
    pub external_memory_priority_preserved: bool,
    pub unrestored_ownership: ProcessEfficiencyOwnership,
    pub property_errors: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InteractiveActivity {
    pub session_id: u32,
    pub foreground_pid: Option<u32>,
    pub visible_pids: Vec<u32>,
    pub audible_pids: Vec<u32>,
    pub window_probe_available: bool,
    pub audio_probe_available: bool,
}

impl MutationReport {
    #[must_use]
    pub fn preview_apply(
        operation: &str,
        pid: u32,
        previous_cpu_set_ids: Vec<u32>,
        requested_cpu_set_ids: Vec<u32>,
    ) -> Self {
        Self {
            operation: operation.to_owned(),
            pid,
            committed: false,
            observed_cpu_set_ids: previous_cpu_set_ids.clone(),
            previous_cpu_set_ids,
            requested_cpu_set_ids,
        }
    }

    #[must_use]
    pub fn preview_clear(pid: u32, previous_cpu_set_ids: Vec<u32>) -> Self {
        Self::preview_apply("clear", pid, previous_cpu_set_ids, Vec::new())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LaunchReport {
    pub pid: u32,
    pub cpu_set_ids: Vec<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObservedProcess {
    pub key: ProcessKey,
    pub parent_pid: u32,
    pub session_id: Option<u32>,
    pub thread_count: u32,
    pub image_name: String,
    pub image_path: Option<String>,
    pub priority_class: Option<u32>,
    pub cpu_time_100ns: u64,
    pub default_cpu_set_ids: Vec<u32>,
    pub current_domain: Option<LlcDomainKey>,
    pub exclusion: Option<ExclusionReason>,
}

impl ObservedProcess {
    #[must_use]
    pub fn policy_observation(
        &self,
        mode: PlacementMode,
        enforcement: EnforcementMode,
    ) -> ProcessObservation {
        ProcessObservation {
            key: self.key,
            mode,
            enforcement,
            current_domain: self.current_domain,
            assignment_origin: if self.default_cpu_set_ids.is_empty() {
                AssignmentOrigin::None
            } else {
                AssignmentOrigin::External
            },
            refresh_required: false,
            preferred_partition: None,
            exclusion: self.exclusion,
        }
    }
}

pub struct LoadSampler {
    #[cfg(windows)]
    inner: windows::LoadSampler,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct SystemPressureSample {
    pub processor_queue_length: u32,
    pub pages_input_per_second: u64,
    pub total_physical_memory_bytes: u64,
    pub available_physical_memory_bytes: u64,
}

pub struct SystemPressureSampler {
    #[cfg(windows)]
    inner: windows::SystemPressureSampler,
}

pub struct MemoryPressureMonitor {
    #[cfg(windows)]
    inner: windows::MemoryPressureMonitor,
}

pub struct InteractiveStateServer {
    #[cfg(windows)]
    inner: windows::InteractiveStateServer,
}

pub type InteractiveStateWake = Arc<dyn Fn() + Send + Sync + 'static>;

impl InteractiveStateServer {
    #[allow(clippy::needless_pass_by_value)] // Windows worker owns the wake callback.
    pub fn start(
        expected_tray_path: &Path,
        wake: Option<InteractiveStateWake>,
    ) -> Result<Self, PlatformError> {
        #[cfg(windows)]
        {
            Ok(Self {
                inner: windows::InteractiveStateServer::start(expected_tray_path, wake)?,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = expected_tray_path;
            let _ = wake;
            Err(PlatformError::UnsupportedPlatform)
        }
    }

    #[must_use]
    #[cfg_attr(not(windows), allow(clippy::unused_self))]
    pub fn states(&self) -> Vec<InteractiveActivityState> {
        #[cfg(windows)]
        {
            self.inner.states()
        }
        #[cfg(not(windows))]
        {
            Vec::new()
        }
    }
}

impl MemoryPressureMonitor {
    pub fn new() -> Result<Self, PlatformError> {
        #[cfg(windows)]
        {
            Ok(Self {
                inner: windows::MemoryPressureMonitor::new()?,
            })
        }
        #[cfg(not(windows))]
        {
            Err(PlatformError::UnsupportedPlatform)
        }
    }

    #[cfg_attr(not(windows), allow(clippy::unused_self))]
    pub fn is_low(&self) -> Result<bool, PlatformError> {
        #[cfg(windows)]
        {
            self.inner.is_low().map_err(Into::into)
        }
        #[cfg(not(windows))]
        {
            Err(PlatformError::UnsupportedPlatform)
        }
    }
}

impl SystemPressureSampler {
    pub fn new() -> Result<Self, PlatformError> {
        #[cfg(windows)]
        {
            Ok(Self {
                inner: windows::SystemPressureSampler::new()?,
            })
        }
        #[cfg(not(windows))]
        {
            Err(PlatformError::UnsupportedPlatform)
        }
    }

    #[cfg_attr(not(windows), allow(clippy::unused_self))]
    pub fn prime(&mut self) -> Result<(), PlatformError> {
        #[cfg(windows)]
        {
            self.inner.prime().map_err(Into::into)
        }
        #[cfg(not(windows))]
        {
            Err(PlatformError::UnsupportedPlatform)
        }
    }

    #[cfg_attr(not(windows), allow(clippy::unused_self))]
    pub fn sample(&mut self) -> Result<SystemPressureSample, PlatformError> {
        #[cfg(windows)]
        {
            self.inner.sample().map_err(Into::into)
        }
        #[cfg(not(windows))]
        {
            Err(PlatformError::UnsupportedPlatform)
        }
    }
}

impl LoadSampler {
    pub fn new(topology: &Topology) -> Result<Self, PlatformError> {
        #[cfg(windows)]
        {
            Ok(Self {
                inner: windows::LoadSampler::new(topology)?,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = topology;
            Err(PlatformError::UnsupportedPlatform)
        }
    }

    #[cfg_attr(not(windows), allow(clippy::unused_self))]
    pub fn prime(&mut self) -> Result<(), PlatformError> {
        #[cfg(windows)]
        {
            self.inner.prime().map_err(Into::into)
        }
        #[cfg(not(windows))]
        {
            Err(PlatformError::UnsupportedPlatform)
        }
    }

    #[cfg_attr(not(windows), allow(clippy::unused_self))]
    pub fn sample(&mut self) -> Result<Vec<DomainLoad>, PlatformError> {
        #[cfg(windows)]
        {
            self.inner.sample().map_err(Into::into)
        }
        #[cfg(not(windows))]
        {
            Err(PlatformError::UnsupportedPlatform)
        }
    }
}

#[cfg(windows)]
pub fn system_topology() -> Result<Topology, PlatformError> {
    windows::system_topology().map_err(Into::into)
}

#[cfg(not(windows))]
pub fn system_topology() -> Result<Topology, PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn inspect_process(pid: u32) -> Result<ProcessSnapshot, PlatformError> {
    windows::inspect_process(pid).map_err(Into::into)
}

#[cfg(not(windows))]
pub fn inspect_process(_pid: u32) -> Result<ProcessSnapshot, PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn inspect_process_efficiency(pid: u32) -> Result<ProcessEfficiencySnapshot, PlatformError> {
    windows::inspect_process_efficiency(pid).map_err(Into::into)
}

#[cfg(not(windows))]
pub fn inspect_process_efficiency(_pid: u32) -> Result<ProcessEfficiencySnapshot, PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn apply_process(pid: u32, cpu_set_ids: &[u32]) -> Result<MutationReport, PlatformError> {
    windows::apply_process(pid, cpu_set_ids).map_err(Into::into)
}

#[cfg(not(windows))]
pub fn apply_process(_pid: u32, _cpu_set_ids: &[u32]) -> Result<MutationReport, PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn clear_process(pid: u32) -> Result<MutationReport, PlatformError> {
    windows::clear_process(pid).map_err(Into::into)
}

#[cfg(not(windows))]
pub fn clear_process(_pid: u32) -> Result<MutationReport, PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn apply_process_key(
    process: ProcessKey,
    cpu_set_ids: &[u32],
) -> Result<MutationReport, PlatformError> {
    windows::apply_process_key(process, cpu_set_ids).map_err(Into::into)
}

#[cfg(not(windows))]
pub fn apply_process_key(
    _process: ProcessKey,
    _cpu_set_ids: &[u32],
) -> Result<MutationReport, PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn clear_process_key(process: ProcessKey) -> Result<MutationReport, PlatformError> {
    windows::clear_process_key(process).map_err(Into::into)
}

#[cfg(not(windows))]
pub fn clear_process_key(_process: ProcessKey) -> Result<MutationReport, PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn query_process_efficiency_key(
    process: ProcessKey,
) -> Result<ProcessEfficiencyState, PlatformError> {
    windows::query_process_efficiency_key(process).map_err(Into::into)
}

#[cfg(not(windows))]
pub fn query_process_efficiency_key(
    _process: ProcessKey,
) -> Result<ProcessEfficiencyState, PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn apply_process_efficiency_key(
    process: ProcessKey,
    expected: ProcessEfficiencyState,
    requested: ProcessEfficiencyState,
    ownership: ProcessEfficiencyOwnership,
) -> Result<EfficiencyMutationReport, PlatformError> {
    windows::apply_process_efficiency_key(process, expected, requested, ownership)
        .map_err(Into::into)
}

#[cfg(not(windows))]
pub fn apply_process_efficiency_key(
    _process: ProcessKey,
    _expected: ProcessEfficiencyState,
    _requested: ProcessEfficiencyState,
    _ownership: ProcessEfficiencyOwnership,
) -> Result<EfficiencyMutationReport, PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn restore_process_efficiency_key(
    process: ProcessKey,
    original: ProcessEfficiencyState,
    applied: ProcessEfficiencyState,
    ownership: ProcessEfficiencyOwnership,
    pending: Option<ProcessEfficiencyState>,
) -> Result<EfficiencyMutationReport, PlatformError> {
    windows::restore_process_efficiency_key(process, original, applied, ownership, pending)
        .map_err(Into::into)
}

#[cfg(not(windows))]
pub fn restore_process_efficiency_key(
    _process: ProcessKey,
    _original: ProcessEfficiencyState,
    _applied: ProcessEfficiencyState,
    _ownership: ProcessEfficiencyOwnership,
    _pending: Option<ProcessEfficiencyState>,
) -> Result<EfficiencyMutationReport, PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn capture_interactive_activity() -> Result<InteractiveActivity, PlatformError> {
    windows::capture_interactive_activity().map_err(Into::into)
}

#[cfg(not(windows))]
pub fn capture_interactive_activity() -> Result<InteractiveActivity, PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn current_session_id() -> Result<u32, PlatformError> {
    windows::current_session_id().map_err(Into::into)
}

#[cfg(not(windows))]
pub fn current_session_id() -> Result<u32, PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn current_process_key() -> Result<ProcessKey, PlatformError> {
    windows::current_process_key().map_err(Into::into)
}

#[cfg(windows)]
pub fn atomic_replace_file(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    windows::atomic_replace_file(path, bytes)
}

#[cfg(not(windows))]
pub fn current_process_key() -> Result<ProcessKey, PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn run_assigned(
    program: &Path,
    args: &[OsString],
    cpu_set_ids: &[u32],
) -> Result<LaunchReport, PlatformError> {
    windows::run_assigned(program, args, cpu_set_ids).map_err(Into::into)
}

#[cfg(windows)]
pub fn observe_processes(topology: &Topology) -> Result<Vec<ObservedProcess>, PlatformError> {
    windows::observe_processes(topology).map_err(Into::into)
}

#[cfg(not(windows))]
pub fn observe_processes(_topology: &Topology) -> Result<Vec<ObservedProcess>, PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

#[cfg(not(windows))]
pub fn run_assigned(
    _program: &Path,
    _args: &[OsString],
    _cpu_set_ids: &[u32],
) -> Result<LaunchReport, PlatformError> {
    Err(PlatformError::UnsupportedPlatform)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn efficiency_ownership_masks_compare_only_owned_properties() {
        let expected = ProcessEfficiencyState {
            eco_qos: ProcessEcoQosState::Enabled,
            memory_priority: ProcessMemoryPriority::BelowNormal,
        };
        let external_memory_change = ProcessEfficiencyState {
            eco_qos: ProcessEcoQosState::Enabled,
            memory_priority: ProcessMemoryPriority::Normal,
        };
        let eco_only = ProcessEfficiencyOwnership {
            eco_qos: true,
            memory_priority: false,
        };

        assert!(eco_only.matches(expected, external_memory_change));
        assert!(!ProcessEfficiencyOwnership::between(expected, external_memory_change).is_empty());
        assert!(eco_only.without(eco_only).is_empty());
    }
}
