//! Shared, serialized control and status contract for the service and tray UI.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use winsched_config::{
    BackgroundEfficiencyConfig, ControllerConfig, ControllerMode, LoggingConfig,
    ResponsivenessConfig,
};
use winsched_core::SystemReservePlan;
use winsched_core::latency::SchedulerLatencyStatus;
use winsched_core::responsiveness::ResponsivenessPressure;

pub const SERVICE_NAME: &str = "WinSched";
pub const INSTALL_DIRECTORY_NAME: &str = "WinSched";
pub const CONFIG_FILE_NAME: &str = "winsched.toml";
pub const LOG_FILE_NAME: &str = "winsched.log";
pub const MANAGED_STATE_FILE_NAME: &str = "managed-state.json";
pub const BACKGROUND_STATE_FILE_NAME: &str = "background-state.json";
pub const INTERACTIVE_PIPE_NAME: &str = r"\\.\pipe\WinSchedInteractive-v1";
pub const RUNTIME_STATE_FILE_NAME: &str = "runtime-state.json";
pub const STATUS_FILE_NAME: &str = "status.json";
pub const CONTROL_ENABLE: u32 = 128;
pub const CONTROL_DISABLE: u32 = 129;
pub const RUNTIME_SCHEMA_VERSION: u32 = 1;
pub const STATUS_SCHEMA_VERSION: u32 = 5;
pub const INTERACTIVE_STATE_SCHEMA_VERSION: u32 = 1;
pub const INTERACTIVE_STATE_HEARTBEAT_MS: u64 = 5_000;
pub const INTERACTIVE_STATE_STALE_AFTER_MS: u64 = 15_000;

/// Untrusted, per-session veto signals published by the unelevated tray.
///
/// The service may use this document only to avoid or undo a mutation. It must
/// never use it to select a mutation target.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractiveActivityState {
    pub schema_version: u32,
    pub session_id: u32,
    pub source_pid: u32,
    pub source_creation_time_100ns: u64,
    pub window_probe_available: bool,
    pub audio_probe_available: bool,
    pub foreground_pid: Option<u32>,
    pub visible_pids: Vec<u32>,
    pub audible_pids: Vec<u32>,
    pub updated_at_unix_ms: u64,
}

impl InteractiveActivityState {
    #[must_use]
    pub fn is_fresh_at(&self, now_unix_ms: u64) -> bool {
        self.schema_version == INTERACTIVE_STATE_SCHEMA_VERSION
            && self.updated_at_unix_ms <= now_unix_ms
            && now_unix_ms.saturating_sub(self.updated_at_unix_ms)
                <= INTERACTIVE_STATE_STALE_AFTER_MS
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackgroundEfficiencyStatus {
    pub eligible_processes: usize,
    pub managed_processes: usize,
    pub protected_processes: usize,
    pub required_probe_sessions: usize,
    pub interactive_probe_sessions: usize,
    pub memory_pressure_monitor_available: bool,
    pub low_memory_condition: bool,
    pub last_action: Option<String>,
}

/// Bounded timing and population metrics for completed controller evaluations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EvaluationTelemetry {
    pub completed_total: u64,
    pub last_duration_us: u64,
    pub rolling_mean_us: u64,
    pub rolling_p95_us: u64,
    pub rolling_max_us: u64,
    pub window_samples: usize,
    pub last_scanned_processes: usize,
    pub last_eligible_processes: usize,
    pub last_decisions: usize,
}

/// Cumulative platform mutation operation outcomes for the current service instance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MutationTelemetry {
    pub placement_attempted: u64,
    pub placement_succeeded: u64,
    pub placement_failed: u64,
    pub background_attempted: u64,
    pub background_succeeded: u64,
    pub background_failed: u64,
}

/// Logical file-sink traffic for the current service instance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingTelemetry {
    pub records_written: u64,
    pub bytes_written: u64,
    pub write_errors: u64,
    pub status_writes: u64,
}

/// Privacy-safe resource snapshot for the service process.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceProcessTelemetry {
    pub uptime_ms: u64,
    pub cpu_time_100ns: u64,
    pub working_set_bytes: u64,
}

/// Optional self-observability payload introduced by status schema 5.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ControllerTelemetry {
    pub evaluation: EvaluationTelemetry,
    pub mutations: MutationTelemetry,
    pub logging: LoggingTelemetry,
    pub service_process: Option<ServiceProcessTelemetry>,
}

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

/// Result of the most recent configuration reload attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigReloadResult {
    Initial,
    Reloaded,
    Rejected,
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
    pub config_reload_sequence: u64,
    pub config_reload_result: ConfigReloadResult,
    pub config_reload_error: Option<String>,
    pub applied_config_fingerprint: u64,
    pub applied_logging: LoggingConfig,
    pub applied_background_efficiency: BackgroundEfficiencyConfig,
    pub applied_responsiveness: ResponsivenessConfig,
    pub background_efficiency: BackgroundEfficiencyStatus,
    pub system_reserve: SystemReservePlan,
    pub scheduler_latency: SchedulerLatencyStatus,
    pub maximum_dpc_time_bps: u16,
    pub maximum_interrupt_time_bps: u16,
    pub memory_profile_physical_cores: u16,
    pub responsiveness_pressure: ResponsivenessPressure,
    pub last_responsiveness_adjustment: Option<String>,
    pub iteration: u64,
    pub managed_processes: usize,
    pub llc_domains: usize,
    pub last_activity: Option<String>,
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<ControllerTelemetry>,
    pub updated_at_unix_ms: u64,
}

impl ControllerStatus {
    #[must_use]
    pub fn starting(
        service_pid: u32,
        scheduling_enabled: bool,
        config: &ControllerConfig,
        system_reserve: SystemReservePlan,
        llc_domains: usize,
        updated_at_unix_ms: u64,
    ) -> Self {
        Self {
            schema_version: STATUS_SCHEMA_VERSION,
            phase: ControllerPhase::Starting,
            service_pid,
            scheduling_enabled,
            configured_mode: config.controller_mode,
            config_reload_sequence: 0,
            config_reload_result: ConfigReloadResult::Initial,
            config_reload_error: None,
            applied_config_fingerprint: config.fingerprint(),
            applied_logging: config.logging,
            applied_background_efficiency: config.background_efficiency,
            applied_responsiveness: config.responsiveness,
            background_efficiency: BackgroundEfficiencyStatus::default(),
            system_reserve,
            scheduler_latency: SchedulerLatencyStatus::default(),
            maximum_dpc_time_bps: 0,
            maximum_interrupt_time_bps: 0,
            memory_profile_physical_cores: config.responsiveness.memory.maximum_physical_cores,
            responsiveness_pressure: ResponsivenessPressure::Unknown,
            last_responsiveness_adjustment: None,
            iteration: 0,
            managed_processes: 0,
            llc_domains,
            last_activity: None,
            last_error: None,
            telemetry: None,
            updated_at_unix_ms,
        }
    }

    /// Returns whether this is a fresh, completed reload attempt after a Settings baseline.
    #[must_use]
    pub fn is_reload_receipt_after(
        &self,
        baseline: Option<(u32, u64)>,
        not_before_unix_ms: u64,
    ) -> bool {
        if self.schema_version != STATUS_SCHEMA_VERSION
            || self.updated_at_unix_ms < not_before_unix_ms
            || self.config_reload_sequence == 0
        {
            return false;
        }
        baseline.is_none_or(|(pid, sequence)| {
            self.service_pid != pid || self.config_reload_sequence > sequence
        })
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
        let config = ControllerConfig {
            controller_mode: ControllerMode::Auto,
            ..ControllerConfig::default()
        };
        let mut status =
            ControllerStatus::starting(42, false, &config, SystemReservePlan::default(), 2, 100);
        status.phase = ControllerPhase::Disabled;
        assert!(!status.scheduling_enabled);
        assert_eq!(status.configured_mode, ControllerMode::Auto);
        assert_eq!(status.config_reload_sequence, 0);
        assert_eq!(status.config_reload_result, ConfigReloadResult::Initial);
        assert_eq!(status.config_reload_error, None);
        assert_eq!(status.telemetry, None);
    }

    #[test]
    fn status_telemetry_round_trips_with_schema_five() {
        let mut status = ControllerStatus::starting(
            42,
            true,
            &ControllerConfig::default(),
            SystemReservePlan::default(),
            4,
            1_000,
        );
        status.telemetry = Some(ControllerTelemetry {
            evaluation: EvaluationTelemetry {
                completed_total: 10,
                last_duration_us: 101,
                rolling_mean_us: 91,
                rolling_p95_us: 120,
                rolling_max_us: 140,
                window_samples: 10,
                last_scanned_processes: 82,
                last_eligible_processes: 16,
                last_decisions: 16,
            },
            mutations: MutationTelemetry {
                placement_attempted: 3,
                placement_succeeded: 2,
                placement_failed: 1,
                background_attempted: 1,
                background_succeeded: 1,
                background_failed: 0,
            },
            logging: LoggingTelemetry {
                records_written: 7,
                bytes_written: 700,
                write_errors: 0,
                status_writes: 3,
            },
            service_process: Some(ServiceProcessTelemetry {
                uptime_ms: 60_000,
                cpu_time_100ns: 123_456,
                working_set_bytes: 8 * 1024 * 1024,
            }),
        });

        let serialized = serde_json::to_vec(&status).unwrap();
        let decoded = serde_json::from_slice::<ControllerStatus>(&serialized).unwrap();
        assert_eq!(decoded.schema_version, STATUS_SCHEMA_VERSION);
        assert_eq!(decoded, status);
    }

    #[test]
    fn status_without_telemetry_defaults_to_unavailable() {
        let status = ControllerStatus::starting(
            42,
            false,
            &ControllerConfig::default(),
            SystemReservePlan::default(),
            2,
            1_000,
        );
        let mut serialized = serde_json::to_value(status).unwrap();
        assert!(serialized.get("telemetry").is_none());
        serialized.as_object_mut().unwrap().remove("telemetry");

        let decoded = serde_json::from_value::<ControllerStatus>(serialized).unwrap();
        assert_eq!(decoded.telemetry, None);
    }

    #[test]
    fn telemetry_substructures_tolerate_future_additive_metrics() {
        let decoded = serde_json::from_value::<ControllerTelemetry>(serde_json::json!({
            "evaluation": {
                "completed_total": 1,
                "future_duration_stat": 99
            },
            "future_category": {
                "value": 1
            }
        }))
        .unwrap();

        assert_eq!(decoded.evaluation.completed_total, 1);
        assert_eq!(decoded.evaluation.last_duration_us, 0);
        assert_eq!(decoded.mutations, MutationTelemetry::default());
        assert_eq!(decoded.logging, LoggingTelemetry::default());
        assert_eq!(decoded.service_process, None);
    }

    #[test]
    fn reload_receipt_freshness_handles_missing_status_and_service_restart() {
        let mut status = ControllerStatus::starting(
            42,
            false,
            &ControllerConfig::default(),
            SystemReservePlan::default(),
            2,
            1_000,
        );
        assert!(!status.is_reload_receipt_after(None, 1_000));

        status.config_reload_sequence = 1;
        status.config_reload_result = ConfigReloadResult::Reloaded;
        assert!(status.is_reload_receipt_after(None, 1_000));
        assert!(!status.is_reload_receipt_after(Some((42, 1)), 1_000));

        status.config_reload_sequence = 2;
        assert!(status.is_reload_receipt_after(Some((42, 1)), 1_000));
        status.service_pid = 84;
        status.config_reload_sequence = 1;
        assert!(status.is_reload_receipt_after(Some((42, 99)), 1_000));
        assert!(!status.is_reload_receipt_after(Some((42, 99)), 1_001));
    }

    #[test]
    fn interactive_activity_rejects_stale_future_and_wrong_schema() {
        let mut state = InteractiveActivityState {
            schema_version: INTERACTIVE_STATE_SCHEMA_VERSION,
            session_id: 2,
            source_pid: 42,
            source_creation_time_100ns: 100,
            window_probe_available: true,
            audio_probe_available: true,
            foreground_pid: Some(77),
            visible_pids: vec![77],
            audible_pids: Vec::new(),
            updated_at_unix_ms: 1_000,
        };
        assert!(state.is_fresh_at(1_000 + INTERACTIVE_STATE_STALE_AFTER_MS));
        assert!(!state.is_fresh_at(1_001 + INTERACTIVE_STATE_STALE_AFTER_MS));
        assert!(!state.is_fresh_at(999));
        state.schema_version += 1;
        assert!(!state.is_fresh_at(1_000));
    }
}
