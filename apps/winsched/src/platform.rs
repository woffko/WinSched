#![allow(clippy::missing_errors_doc)] // Thin facade preserves detailed PlatformError values.

use std::ffi::OsString;
use std::path::Path;

use serde::Serialize;
use thiserror::Error;
use winsched_core::{
    LlcDomainKey, Topology,
    adaptive::{
        AssignmentOrigin, DomainLoad, EnforcementMode, ExclusionReason, PlacementMode, ProcessKey,
        ProcessObservation,
    },
};

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

#[derive(Debug, Clone, Serialize)]
pub struct ProcessSnapshot {
    pub pid: u32,
    pub default_cpu_set_ids: Vec<u32>,
    pub topology: Topology,
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
