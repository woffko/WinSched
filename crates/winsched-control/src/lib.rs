//! Shared, serialized control and status contract for the service and tray UI.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use winsched_config::ControllerMode;

pub const SERVICE_NAME: &str = "WinSched";
pub const INSTALL_DIRECTORY_NAME: &str = "WinSched";
pub const CONFIG_FILE_NAME: &str = "winsched.toml";
pub const LOG_FILE_NAME: &str = "winsched.log";
pub const MANAGED_STATE_FILE_NAME: &str = "managed-state.json";
pub const RUNTIME_STATE_FILE_NAME: &str = "runtime-state.json";
pub const STATUS_FILE_NAME: &str = "status.json";
pub const CONTROL_ENABLE: u32 = 128;
pub const CONTROL_DISABLE: u32 = 129;
pub const RUNTIME_SCHEMA_VERSION: u32 = 1;
pub const STATUS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerPhase {
    Starting,
    Running,
    Disabled,
    Stopping,
    Stopped,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeState {
    pub schema_version: u32,
    pub scheduling_enabled: bool,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self::for_controller_mode(ControllerMode::Observe)
    }
}

impl RuntimeState {
    #[must_use]
    pub const fn for_controller_mode(mode: ControllerMode) -> Self {
        Self {
            schema_version: RUNTIME_SCHEMA_VERSION,
            scheduling_enabled: matches!(mode, ControllerMode::Auto),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerStatus {
    pub schema_version: u32,
    pub phase: ControllerPhase,
    pub service_pid: u32,
    pub scheduling_enabled: bool,
    pub configured_mode: ControllerMode,
    pub iteration: u64,
    pub managed_processes: usize,
    pub llc_domains: usize,
    pub last_activity: Option<String>,
    pub last_error: Option<String>,
    pub updated_at_unix_ms: u64,
}

impl ControllerStatus {
    #[must_use]
    pub const fn starting(
        service_pid: u32,
        scheduling_enabled: bool,
        configured_mode: ControllerMode,
        llc_domains: usize,
        updated_at_unix_ms: u64,
    ) -> Self {
        Self {
            schema_version: STATUS_SCHEMA_VERSION,
            phase: ControllerPhase::Starting,
            service_pid,
            scheduling_enabled,
            configured_mode,
            iteration: 0,
            managed_processes: 0,
            llc_domains,
            last_activity: None,
            last_error: None,
            updated_at_unix_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_defaults_fail_closed_with_current_schema() {
        let state = RuntimeState::default();
        assert_eq!(state.schema_version, RUNTIME_SCHEMA_VERSION);
        assert!(!state.scheduling_enabled);
        assert!(RuntimeState::for_controller_mode(ControllerMode::Auto).scheduling_enabled);
    }

    #[test]
    fn disabled_status_can_be_serialized_by_consumers() {
        let mut status = ControllerStatus::starting(42, false, ControllerMode::Auto, 2, 100);
        status.phase = ControllerPhase::Disabled;
        assert!(!status.scheduling_enabled);
        assert_eq!(status.configured_mode, ControllerMode::Auto);
    }
}
