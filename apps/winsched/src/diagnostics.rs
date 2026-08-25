//! Privacy-safe, read-only Windows responsiveness diagnostics.

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use winsched_control::ControllerPhase;
use winsched_core::latency::SchedulerLatencyStatus;
use winsched_core::responsiveness::ResponsivenessPressure;

#[cfg(windows)]
mod windows;

pub const DIAGNOSTIC_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_DURATION: Duration = Duration::from_secs(10);
pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(250);
pub const DEFAULT_TASKBAR_TIMEOUT: Duration = Duration::from_millis(50);

#[cfg(any(windows, test))]
const CPU_SPARE_BPS: u16 = 5_000;
#[cfg(any(windows, test))]
const CPU_SATURATED_BPS: u16 = 8_500;
#[cfg(any(windows, test))]
const DPC_OR_INTERRUPT_PRESSURE_BPS: u16 = 500;
#[cfg(any(windows, test))]
const SCHEDULER_PRESSURE_P99_US: u64 = 2_000;
#[cfg(any(windows, test))]
const TASKBAR_DEGRADED_P95_US: u64 = 100_000;
#[cfg(any(windows, test))]
const MEMORY_PRESSURE_PERCENT: u64 = 10;
#[cfg(any(windows, test))]
const EXPLORER_PROCESS_FANOUT: u32 = 8;
#[cfg(any(windows, test))]
const EXPLORER_WINDOW_FANOUT: u32 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticOptions {
    pub duration: Duration,
    pub interval: Duration,
    pub taskbar_timeout: Duration,
}

impl Default for DiagnosticOptions {
    fn default() -> Self {
        Self {
            duration: DEFAULT_DURATION,
            interval: DEFAULT_INTERVAL,
            taskbar_timeout: DEFAULT_TASKBAR_TIMEOUT,
        }
    }
}

#[derive(Debug, Error)]
pub enum DiagnosticError {
    #[error("diagnostic duration must be between 1 and 120 seconds")]
    InvalidDuration,
    #[error("diagnostic interval must be between 100 and 5000 milliseconds")]
    InvalidInterval,
    #[error("taskbar timeout must be between 10 and 250 milliseconds")]
    InvalidTaskbarTimeout,
    #[error(transparent)]
    Platform(#[from] crate::platform::PlatformError),
    #[error("failed to start scheduler-latency probe: {0}")]
    LatencyProbe(#[from] std::io::Error),
    #[error("diagnostic cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Information,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticFindingCode {
    Healthy,
    CpuSaturation,
    SchedulerLatency,
    DpcOrInterruptPressure,
    MemoryPressure,
    ShellLatencyWithSpareCpu,
    ExplorerFanout,
    WslResourcePressure,
    ServiceStatusUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticFinding {
    pub code: DiagnosticFindingCode,
    pub severity: DiagnosticSeverity,
    pub summary: String,
    pub recommendation: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemDiagnostic {
    pub logical_processors: u32,
    pub physical_cores: u32,
    pub average_cpu_utilization_bps: u16,
    pub maximum_domain_utilization_bps: u16,
    pub maximum_processor_queue_length: u32,
    pub maximum_dpc_time_bps: u16,
    pub maximum_interrupt_time_bps: u16,
    pub maximum_pages_input_per_second: u64,
    pub total_physical_memory_bytes: u64,
    pub minimum_available_memory_bytes: u64,
    pub scheduler_latency: SchedulerLatencyStatus,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskbarDiagnostic {
    pub available: bool,
    pub samples: u32,
    pub successful_samples: u32,
    pub timeout_samples: u32,
    pub p50_response_us: u64,
    pub p95_response_us: u64,
    pub maximum_response_us: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellDiagnostic {
    pub taskbar: TaskbarDiagnostic,
    pub explorer_processes: u32,
    pub explorer_threads: u32,
    pub explorer_windows: u32,
    pub launch_folders_in_separate_process: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WslConfigDiagnostic {
    pub present: bool,
    pub memory_bytes: Option<u64>,
    pub processors: Option<u32>,
    pub swap_bytes: Option<u64>,
    pub auto_memory_reclaim: Option<String>,
    pub maximum_crash_dump_count: Option<u32>,
    pub recognized_values: u32,
    pub malformed_recognized_values: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VirtualizationDiagnostic {
    pub wsl_processes: u32,
    pub wsl_threads: u32,
    pub vmware_vm_processes: u32,
    pub vmware_vm_threads: u32,
    pub wsl_config: WslConfigDiagnostic,
    pub wsl_advice: WslAdviceDiagnostic,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WslAdviceDiagnostic {
    pub resource_pressure_observed: bool,
    pub recommended_memory_bytes: Option<u64>,
    pub recommended_processors: Option<u32>,
    pub requires_wsl_restart: bool,
    pub automatic_changes_performed: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceDiagnostic {
    pub available: bool,
    pub schema_version: Option<u32>,
    pub phase: Option<ControllerPhase>,
    pub scheduling_enabled: Option<bool>,
    pub responsiveness_pressure: Option<ResponsivenessPressure>,
    pub scheduler_latency_p99_us: Option<u64>,
    pub status_age_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticReport {
    pub schema_version: u32,
    pub captured_at_unix_ms: u64,
    pub duration_ms: u64,
    pub sample_count: u32,
    pub system: SystemDiagnostic,
    pub shell: ShellDiagnostic,
    pub virtualization: VirtualizationDiagnostic,
    pub service: ServiceDiagnostic,
    pub findings: Vec<DiagnosticFinding>,
}

/// Runs the bounded diagnostic in the interactive caller's Windows session.
///
/// # Errors
///
/// Returns invalid-option, platform telemetry, or probe-thread errors.
pub fn run(options: DiagnosticOptions) -> Result<DiagnosticReport, DiagnosticError> {
    validate_options(options)?;
    #[cfg(windows)]
    {
        windows::run(options, None)
    }
    #[cfg(not(windows))]
    {
        let _ = options;
        Err(crate::platform::PlatformError::UnsupportedPlatform.into())
    }
}

/// Runs the diagnostic with a cooperative cancellation flag checked between samples.
///
/// # Errors
///
/// Returns [`DiagnosticError::Cancelled`] after cancellation or any normal diagnostic error.
pub fn run_cancellable(
    options: DiagnosticOptions,
    cancellation: &AtomicBool,
) -> Result<DiagnosticReport, DiagnosticError> {
    validate_options(options)?;
    #[cfg(windows)]
    {
        windows::run(options, Some(cancellation))
    }
    #[cfg(not(windows))]
    {
        let _ = (options, cancellation);
        Err(crate::platform::PlatformError::UnsupportedPlatform.into())
    }
}

fn validate_options(options: DiagnosticOptions) -> Result<(), DiagnosticError> {
    if !(Duration::from_secs(1)..=Duration::from_mins(2)).contains(&options.duration) {
        return Err(DiagnosticError::InvalidDuration);
    }
    if !(Duration::from_millis(100)..=Duration::from_secs(5)).contains(&options.interval) {
        return Err(DiagnosticError::InvalidInterval);
    }
    if !(Duration::from_millis(10)..=Duration::from_millis(250)).contains(&options.taskbar_timeout)
    {
        return Err(DiagnosticError::InvalidTaskbarTimeout);
    }
    Ok(())
}

#[cfg(any(windows, test))]
pub(crate) fn classify(report: &mut DiagnosticReport) {
    let mut findings = Vec::new();
    let system = report.system;
    let taskbar = report.shell.taskbar;
    let queue_pressure_threshold = system.logical_processors.div_ceil(4).max(2);
    let cpu_saturated = system.average_cpu_utilization_bps >= CPU_SATURATED_BPS
        || system.maximum_processor_queue_length >= queue_pressure_threshold;
    let scheduler_pressure = system.scheduler_latency.p99_lateness_us > SCHEDULER_PRESSURE_P99_US
        && system.scheduler_latency.window_samples >= 100;
    let dpc_pressure = system.maximum_dpc_time_bps > DPC_OR_INTERRUPT_PRESSURE_BPS
        || system.maximum_interrupt_time_bps > DPC_OR_INTERRUPT_PRESSURE_BPS;
    let memory_pressure = system.total_physical_memory_bytes > 0
        && system.minimum_available_memory_bytes.saturating_mul(100)
            <= system
                .total_physical_memory_bytes
                .saturating_mul(MEMORY_PRESSURE_PERCENT);
    let taskbar_degraded = taskbar.available
        && (taskbar.timeout_samples >= 2 || taskbar.p95_response_us >= TASKBAR_DEGRADED_P95_US);

    if cpu_saturated {
        findings.push(finding(
            DiagnosticFindingCode::CpuSaturation,
            DiagnosticSeverity::Warning,
            "CPU capacity or runnable-queue pressure is elevated.",
            "Reduce or contain compute-heavy workloads before changing shell placement.",
        ));
    }
    if scheduler_pressure {
        findings.push(finding(
            DiagnosticFindingCode::SchedulerLatency,
            DiagnosticSeverity::Warning,
            "Normal-priority scheduler wake latency is elevated.",
            "Inspect sustained CPU, virtualization, DPC, and interrupt pressure.",
        ));
    }
    if dpc_pressure {
        findings.push(finding(
            DiagnosticFindingCode::DpcOrInterruptPressure,
            DiagnosticSeverity::Warning,
            "DPC or interrupt processing is elevated.",
            "Investigate drivers and devices before applying CPU Set policy changes.",
        ));
    }
    if memory_pressure {
        findings.push(finding(
            DiagnosticFindingCode::MemoryPressure,
            DiagnosticSeverity::Warning,
            "Available physical memory is low.",
            "Reduce memory pressure and inspect hard-fault activity.",
        ));
    }
    if taskbar_degraded
        && system.average_cpu_utilization_bps <= CPU_SPARE_BPS
        && !scheduler_pressure
        && !dpc_pressure
    {
        findings.push(finding(
            DiagnosticFindingCode::ShellLatencyWithSpareCpu,
            DiagnosticSeverity::Warning,
            "The taskbar is responding slowly while CPU capacity remains available.",
            "Inspect Explorer integrations and GUI clients; additional reserved cores are unlikely to fix this condition.",
        ));
    }
    if report.shell.explorer_processes >= EXPLORER_PROCESS_FANOUT
        || report.shell.explorer_windows >= EXPLORER_WINDOW_FANOUT
    {
        findings.push(finding(
            DiagnosticFindingCode::ExplorerFanout,
            DiagnosticSeverity::Information,
            "Many Explorer processes or folder windows are active.",
            "Treat this as context and test fewer windows or shell extensions; do not change SeparateProcess automatically.",
        ));
    }
    apply_wsl_advice(
        report,
        system,
        cpu_saturated,
        memory_pressure,
        &mut findings,
    );
    if !report.service.available {
        findings.push(finding(
            DiagnosticFindingCode::ServiceStatusUnavailable,
            DiagnosticSeverity::Information,
            "WinSched service status is unavailable.",
            "Start or update the service to include live policy and scheduler-latency context.",
        ));
    }
    if findings.is_empty() {
        findings.push(finding(
            DiagnosticFindingCode::Healthy,
            DiagnosticSeverity::Information,
            "No supported responsiveness pressure signal was detected.",
            "Repeat the bounded diagnostic while the symptom is occurring if delays persist.",
        ));
    }
    report.findings = findings;
}

#[cfg(any(windows, test))]
fn apply_wsl_advice(
    report: &mut DiagnosticReport,
    system: SystemDiagnostic,
    cpu_saturated: bool,
    memory_pressure: bool,
    findings: &mut Vec<DiagnosticFinding>,
) {
    if report.virtualization.wsl_processes == 0 || (!cpu_saturated && !memory_pressure) {
        return;
    }
    let recommended_memory_bytes =
        (memory_pressure && report.virtualization.wsl_config.memory_bytes.is_none()).then(|| {
            system
                .total_physical_memory_bytes
                .saturating_div(8)
                .saturating_mul(3)
                .max(8 * 1024_u64.pow(3))
        });
    let recommended_processors =
        (cpu_saturated && report.virtualization.wsl_config.processors.is_none()).then(|| {
            system
                .logical_processors
                .saturating_mul(3)
                .saturating_div(4)
                .max(2)
        });
    report.virtualization.wsl_advice = WslAdviceDiagnostic {
        resource_pressure_observed: true,
        recommended_memory_bytes,
        recommended_processors,
        requires_wsl_restart: recommended_memory_bytes.is_some()
            || recommended_processors.is_some(),
        automatic_changes_performed: false,
    };
    findings.push(finding(
        DiagnosticFindingCode::WslResourcePressure,
        DiagnosticSeverity::Information,
        "WSL is active while the host is under measurable resource pressure.",
        "Review .wslconfig memory or processor limits; never apply process CPU Sets to vmmemWSL.",
    ));
}

#[cfg(any(windows, test))]
fn finding(
    code: DiagnosticFindingCode,
    severity: DiagnosticSeverity,
    summary: &str,
    recommendation: &str,
) -> DiagnosticFinding {
    DiagnosticFinding {
        code,
        severity,
        summary: summary.to_owned(),
        recommendation: recommendation.to_owned(),
    }
}

#[cfg(any(windows, test))]
pub(crate) fn parse_wsl_config(contents: Option<&str>) -> WslConfigDiagnostic {
    let Some(contents) = contents else {
        return WslConfigDiagnostic::default();
    };
    let mut result = WslConfigDiagnostic {
        present: true,
        ..WslConfigDiagnostic::default()
    };
    let mut section = String::new();
    for raw in contents.lines() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_ascii_lowercase();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        let recognized = match (section.as_str(), key.as_str()) {
            ("wsl2", "memory") => set_size(&mut result.memory_bytes, value),
            ("wsl2", "processors") => set_u32(&mut result.processors, value),
            ("wsl2", "swap") => set_size(&mut result.swap_bytes, value),
            ("wsl2", "maxcrashdumpcount") => set_u32(&mut result.maximum_crash_dump_count, value),
            ("experimental", "automemoryreclaim") => {
                let normalized = value.to_ascii_lowercase();
                if matches!(normalized.as_str(), "disabled" | "gradual" | "dropcache") {
                    result.auto_memory_reclaim = Some(normalized);
                    true
                } else {
                    false
                }
            }
            _ => continue,
        };
        result.recognized_values = result.recognized_values.saturating_add(1);
        if !recognized {
            result.malformed_recognized_values =
                result.malformed_recognized_values.saturating_add(1);
        }
    }
    result
}

#[cfg(any(windows, test))]
fn set_u32(target: &mut Option<u32>, value: &str) -> bool {
    value.parse::<u32>().is_ok_and(|parsed| {
        *target = Some(parsed);
        true
    })
}

#[cfg(any(windows, test))]
fn set_size(target: &mut Option<u64>, value: &str) -> bool {
    parse_size_bytes(value).is_some_and(|parsed| {
        *target = Some(parsed);
        true
    })
}

#[cfg(any(windows, test))]
fn parse_size_bytes(value: &str) -> Option<u64> {
    let normalized = value.trim().to_ascii_lowercase();
    let split = normalized
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(normalized.len());
    let amount = normalized[..split].parse::<u64>().ok()?;
    let unit = normalized[split..].trim();
    let multiplier = match unit {
        "" | "b" => 1,
        "kb" => 1024,
        "mb" => 1024 * 1024,
        "gb" => 1024 * 1024 * 1024,
        "tb" => 1024_u64.pow(4),
        _ => return None,
    };
    amount.checked_mul(multiplier)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> DiagnosticReport {
        DiagnosticReport {
            schema_version: DIAGNOSTIC_SCHEMA_VERSION,
            captured_at_unix_ms: 0,
            duration_ms: 10_000,
            sample_count: 40,
            system: SystemDiagnostic {
                logical_processors: 64,
                physical_cores: 32,
                average_cpu_utilization_bps: 900,
                total_physical_memory_bytes: 256 * 1024_u64.pow(3),
                minimum_available_memory_bytes: 150 * 1024_u64.pow(3),
                scheduler_latency: SchedulerLatencyStatus {
                    enabled: true,
                    window_samples: 1_000,
                    p99_lateness_us: 500,
                    ..SchedulerLatencyStatus::default()
                },
                ..SystemDiagnostic::default()
            },
            shell: ShellDiagnostic {
                taskbar: TaskbarDiagnostic {
                    available: true,
                    samples: 40,
                    successful_samples: 36,
                    timeout_samples: 4,
                    p95_response_us: 125_000,
                    maximum_response_us: 250_000,
                    ..TaskbarDiagnostic::default()
                },
                explorer_processes: 17,
                explorer_windows: 24,
                launch_folders_in_separate_process: Some(true),
                ..ShellDiagnostic::default()
            },
            virtualization: VirtualizationDiagnostic::default(),
            service: ServiceDiagnostic {
                available: true,
                ..ServiceDiagnostic::default()
            },
            findings: Vec::new(),
        }
    }

    #[test]
    fn classifies_shell_latency_with_spare_cpu_without_recommending_cpu_sets() {
        let mut report = report();
        classify(&mut report);
        let shell = report
            .findings
            .iter()
            .find(|finding| finding.code == DiagnosticFindingCode::ShellLatencyWithSpareCpu)
            .unwrap();
        assert!(shell.recommendation.contains("unlikely to fix"));
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.code == DiagnosticFindingCode::ExplorerFanout)
        );
    }

    #[test]
    fn cpu_saturation_prevents_spare_cpu_shell_classification() {
        let mut report = report();
        report.system.average_cpu_utilization_bps = 9_000;
        classify(&mut report);
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.code == DiagnosticFindingCode::CpuSaturation)
        );
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.code == DiagnosticFindingCode::ShellLatencyWithSpareCpu)
        );
    }

    #[test]
    fn small_queue_spike_on_many_logical_processors_is_not_saturation() {
        let mut report = report();
        report.shell.taskbar = TaskbarDiagnostic {
            available: true,
            samples: 40,
            successful_samples: 40,
            ..TaskbarDiagnostic::default()
        };
        report.system.maximum_processor_queue_length = 2;
        report.virtualization.wsl_processes = 1;
        classify(&mut report);
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.code == DiagnosticFindingCode::CpuSaturation)
        );
        assert_eq!(
            report.virtualization.wsl_advice,
            WslAdviceDiagnostic::default()
        );
    }

    #[test]
    fn wsl_advice_is_pressure_gated_and_never_applies_changes() {
        let mut pressured = report();
        pressured.virtualization.wsl_processes = 1;
        pressured.system.average_cpu_utilization_bps = 9_000;
        classify(&mut pressured);
        assert_eq!(
            pressured.virtualization.wsl_advice.recommended_processors,
            Some(48)
        );
        assert!(pressured.virtualization.wsl_advice.requires_wsl_restart);
        assert!(
            !pressured
                .virtualization
                .wsl_advice
                .automatic_changes_performed
        );

        let mut quiet = report();
        quiet.virtualization.wsl_processes = 1;
        classify(&mut quiet);
        assert_eq!(
            quiet.virtualization.wsl_advice,
            WslAdviceDiagnostic::default()
        );
    }

    #[test]
    fn parses_only_supported_wsl_values_without_retaining_paths() {
        let parsed = parse_wsl_config(Some(
            "[wsl2]\nmemory=64GB\nprocessors=48\nswap=8GB\nkernel=C:\\\\private\\kernel\nmaxCrashDumpCount=3\n[experimental]\nautoMemoryReclaim=dropCache\n",
        ));
        assert_eq!(parsed.memory_bytes, Some(64 * 1024_u64.pow(3)));
        assert_eq!(parsed.processors, Some(48));
        assert_eq!(parsed.swap_bytes, Some(8 * 1024_u64.pow(3)));
        assert_eq!(parsed.maximum_crash_dump_count, Some(3));
        assert_eq!(parsed.auto_memory_reclaim.as_deref(), Some("dropcache"));
        assert_eq!(parsed.recognized_values, 5);
        let json = serde_json::to_string(&parsed).unwrap();
        assert!(!json.contains("private"));
        assert!(!json.contains("kernel"));
    }

    #[test]
    fn rejects_unbounded_diagnostic_options() {
        assert!(matches!(
            validate_options(DiagnosticOptions {
                duration: Duration::from_millis(500),
                ..DiagnosticOptions::default()
            }),
            Err(DiagnosticError::InvalidDuration)
        ));
    }
}
