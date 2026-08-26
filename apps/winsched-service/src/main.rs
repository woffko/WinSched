#[cfg(any(windows, test))]
mod event_logger;
#[cfg(any(windows, test))]
mod responsiveness_log;
#[cfg(any(windows, test))]
mod status_publisher;

#[cfg(not(windows))]
fn main() {
    eprintln!("winsched-service is only available on Windows");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    match app::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
mod app {
    use crate::event_logger::EventSink;
    use crate::responsiveness_log::{ResponsivenessLogGate, ResponsivenessSignature};
    use crate::status_publisher::StatusPublishGate;
    use std::cmp::Reverse;
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::{OsStr, OsString};
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::num::NonZeroU16;
    use std::os::windows::ffi::OsStringExt;
    use std::path::{Path, PathBuf};
    use std::process::Command as ProcessCommand;
    use std::sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use clap::{Parser, Subcommand};
    use serde::{Deserialize, Serialize};
    use serde_json::{Value, json};
    use thiserror::Error;
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
    use windows_service::Error as WindowsServiceError;
    use windows_service::define_windows_service;
    use windows_service::service::{
        Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceConfig, ServiceControl,
        ServiceControlAccept, ServiceErrorControl, ServiceExitCode, ServiceFailureActions,
        ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState, ServiceStatus,
        ServiceType, UserEventCode,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_dispatcher;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use winsched::platform::{
        self, MutationReport, ProcessEcoQosState, ProcessEfficiencyOwnership,
        ProcessEfficiencyState, ProcessMemoryPriority,
    };
    use winsched_config::{ControllerConfig, ControllerMode, LoggingConfig, WorkloadProfile};
    use winsched_control::{
        BACKGROUND_STATE_FILE_NAME, BackgroundEfficiencyStatus, CONFIG_FILE_NAME, CONTROL_DISABLE,
        CONTROL_ENABLE, ConfigReloadResult, ControllerPhase, ControllerStatus,
        INSTALL_DIRECTORY_NAME, InteractiveActivityState, LOG_FILE_NAME, MANAGED_STATE_FILE_NAME,
        RUNTIME_SCHEMA_VERSION, RUNTIME_STATE_FILE_NAME, RuntimeState, SERVICE_NAME,
        STATUS_FILE_NAME,
    };
    use winsched_core::adaptive::{
        AssignmentOrigin, DecisionReason, PlacementMode, PolicyAction, PolicyDecision,
        PolicyEngine, ProcessKey,
    };
    use winsched_core::latency::SchedulerLatencyProbe;
    use winsched_core::responsiveness::{
        AdaptiveWidthConfig, AdaptiveWidthController, WidthAdjustment,
    };
    use winsched_core::{ProcessorClassPreference, SystemReservePlan, Topology};

    const SERVICE_DISPLAY_NAME: &str = "WinSched LLC-aware placement controller";
    const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;
    const LATENCY_PROBE_INTERVAL: Duration = Duration::from_millis(10);
    const LATENCY_PROBE_WINDOW_SAMPLES: usize = 6_000;
    const LEGACY_STATE_SCHEMA_VERSION: u32 = 1;
    const STATE_SCHEMA_VERSION: u32 = 2;
    const BACKGROUND_STATE_SCHEMA_VERSION: u32 = 2;
    const BACKGROUND_SAFETY_INTERVAL: Duration = Duration::from_secs(1);
    const INTERACTIVE_SERVICE_SDDL: &str = concat!(
        "D:",
        "(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;SY)",
        "(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)",
        "(A;;CCLCSWRPWPLOCRRC;;;IU)"
    );
    static SERVICE_CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

    #[derive(Debug, Default, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ManagedStateFile {
        schema_version: u32,
        processes: Vec<ManagedProcess>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ManagedProcess {
        key: ProcessKey,
        placement: ManagedPlacement,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct ManagedPlacement {
        anchor_domain: winsched_core::LlcDomainKey,
        cpu_set_ids: Vec<u32>,
    }

    type ManagedAssignments = BTreeMap<ProcessKey, ManagedPlacement>;

    #[derive(Debug, Default, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct BackgroundStateFile {
        schema_version: u32,
        processes: Vec<ManagedBackgroundProcess>,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct ManagedBackgroundProcess {
        key: ProcessKey,
        original: ProcessEfficiencyState,
        applied: ProcessEfficiencyState,
        #[serde(default)]
        ownership: ProcessEfficiencyOwnership,
        #[serde(default)]
        pending: Option<ProcessEfficiencyState>,
        #[serde(default)]
        pending_ownership: Option<ProcessEfficiencyOwnership>,
        #[serde(default)]
        blocked_by_external_override: ProcessEfficiencyOwnership,
    }

    type ManagedBackground = BTreeMap<ProcessKey, ManagedBackgroundProcess>;

    #[derive(Debug, Default, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LegacyBackgroundStateFile {
        #[serde(rename = "schema_version")]
        _schema_version: u32,
        processes: Vec<LegacyManagedBackgroundProcess>,
    }

    #[derive(Debug, Clone, Copy, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LegacyManagedBackgroundProcess {
        key: ProcessKey,
        original: ProcessEfficiencyState,
        applied: ProcessEfficiencyState,
        #[serde(default)]
        pending: Option<ProcessEfficiencyState>,
        #[serde(default)]
        blocked_by_external_override: bool,
    }

    #[derive(Debug, Default, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LegacyManagedStateFile {
        #[serde(rename = "schema_version")]
        _schema_version: u32,
        processes: Vec<LegacyManagedProcess>,
    }

    #[derive(Debug, Clone, Copy, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LegacyManagedProcess {
        key: ProcessKey,
        domain: winsched_core::LlcDomainKey,
    }

    #[derive(Debug, Deserialize)]
    struct ManagedStateHeader {
        schema_version: u32,
    }

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    struct CleanupReport {
        attempted: usize,
        cleared: usize,
        failed: usize,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ControllerCommand {
        Stop,
        Enable,
        Disable,
        Tick,
    }

    #[derive(Debug, Clone, Copy, Default)]
    struct ControllerFiles<'a> {
        config: Option<&'a Path>,
        managed_state: Option<&'a Path>,
        background_state: Option<&'a Path>,
        tray_binary: Option<&'a Path>,
        runtime_state: Option<&'a Path>,
        status: Option<&'a Path>,
    }

    define_windows_service!(ffi_service_main, service_main);

    #[derive(Debug, Parser)]
    #[command(name = "winsched-service", version)]
    struct Cli {
        #[command(subcommand)]
        command: Command,
    }

    #[derive(Debug, Subcommand)]
    enum Command {
        /// Run bounded controller iterations in the current console.
        Console {
            #[arg(long)]
            config: PathBuf,
            #[arg(long, default_value = "5")]
            iterations: NonZeroU16,
        },
        /// Internal SCM entry mode.
        Service {
            #[arg(long)]
            config: PathBuf,
        },
        /// Install the service and a validated configuration under `ProgramData`.
        Install {
            #[arg(long)]
            config: PathBuf,
            #[arg(long)]
            data_directory: Option<PathBuf>,
            #[arg(long)]
            start: bool,
            #[arg(long)]
            allow_auto: bool,
        },
        /// Register this executable in place and use an existing validated configuration.
        Register {
            #[arg(long)]
            config: PathBuf,
            #[arg(long)]
            start: bool,
            #[arg(long)]
            allow_auto: bool,
        },
        /// Create or repair the service registration for this executable in place.
        Provision {
            #[arg(long)]
            config: PathBuf,
            #[arg(long)]
            start: bool,
            #[arg(long)]
            allow_auto: bool,
            #[arg(long, hide = true)]
            test_fail_after_change: bool,
            #[arg(long)]
            result_file: Option<PathBuf>,
        },
        Start,
        Stop,
        Enable,
        Disable,
        Status,
        Uninstall {
            #[arg(long)]
            data_directory: Option<PathBuf>,
        },
    }

    #[derive(Debug, Error)]
    pub(super) enum ServiceError {
        #[error(transparent)]
        Service(#[from] windows_service::Error),
        #[error(transparent)]
        Io(#[from] std::io::Error),
        #[error(transparent)]
        Config(#[from] winsched_config::ConfigError),
        #[error(transparent)]
        Platform(#[from] platform::PlatformError),
        #[error(transparent)]
        Policy(#[from] winsched_core::adaptive::AdaptiveError),
        #[error("failed to serialize TOML configuration: {0}")]
        Toml(#[from] toml::ser::Error),
        #[error("failed to serialize or parse service state: {0}")]
        Json(#[from] serde_json::Error),
        #[error("unsupported managed-state schema {0}")]
        StateSchema(u32),
        #[error("invalid background-state journal: {0}")]
        InvalidBackgroundState(&'static str),
        #[error("unsupported runtime-state schema {0}")]
        RuntimeStateSchema(u32),
        #[error("controller_mode=auto requires explicit --allow-auto during installation")]
        AutoNeedsConfirmation,
        #[error("service configuration path was not initialized")]
        MissingServiceConfig,
        #[error("failed to restore {0} managed process state record(s)")]
        CleanupIncomplete(usize),
        #[error("service did not stop before the 20-second timeout")]
        ServiceStopTimeout,
        #[error("service stopped after a controller failure: {0}")]
        ServiceStoppedWithError(String),
        #[error("service did not reach Running before the 20-second timeout")]
        ServiceStartTimeout,
        #[error("service registration did not disappear before the 20-second timeout")]
        ServiceDeleteTimeout,
        #[error("existing service configuration cannot be transacted safely: {0}")]
        UnsupportedServiceConfiguration(&'static str),
        #[error("unexpected output from {program}: {detail}")]
        InvalidCommandOutput {
            program: &'static str,
            detail: &'static str,
        },
        #[error("service provisioning failed: {operation}; recovery also failed: {recovery}")]
        TransactionRecovery {
            operation: Box<ServiceError>,
            recovery: String,
        },
        #[error("injected provisioning failure after service configuration change")]
        InjectedProvisionFailure,
        #[error("{program} failed with exit code {exit_code:?}: {stderr}")]
        CommandFailed {
            program: &'static str,
            exit_code: Option<i32>,
            stderr: String,
        },
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum PriorServiceState {
        Running,
        Stopped,
    }

    #[derive(Debug)]
    struct ExistingServiceSnapshot {
        config: ServiceConfig,
        state: PriorServiceState,
        description: OsString,
        failure_actions: ServiceFailureActions,
        failure_actions_on_non_crash: bool,
        sddl: OsString,
    }

    #[derive(Debug)]
    enum ServiceOrigin {
        Created,
        Existing(Box<ExistingServiceSnapshot>),
    }

    pub(super) fn run() -> Result<(), ServiceError> {
        match Cli::parse().command {
            Command::Console { config, iterations } => {
                let parsed = load_config(&config)?;
                let mut logger = EventLogger::console();
                run_controller(
                    parsed,
                    ControllerFiles {
                        config: Some(&config),
                        ..ControllerFiles::default()
                    },
                    None,
                    Some(iterations.get()),
                    None,
                    &mut logger,
                )
            }
            Command::Service { config } => {
                SERVICE_CONFIG_PATH
                    .set(config)
                    .map_err(|_| ServiceError::MissingServiceConfig)?;
                service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
                Ok(())
            }
            Command::Install {
                config,
                data_directory,
                start,
                allow_auto,
            } => install(&config, data_directory.as_deref(), start, allow_auto),
            Command::Register {
                config,
                start,
                allow_auto,
            } => register_in_place(&config, start, allow_auto),
            Command::Provision {
                config,
                start,
                allow_auto,
                test_fail_after_change,
                result_file,
            } => {
                let result = provision_in_place(&config, start, allow_auto, test_fail_after_change);
                if let Some(path) = result_file {
                    let receipt = match &result {
                        Ok(()) => "SUCCESS\n".to_owned(),
                        Err(error) => format!("ERROR: {error}\n"),
                    };
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    if let Err(receipt_error) = atomic_write(&path, receipt.as_bytes())
                        && result.is_ok()
                    {
                        return Err(receipt_error.into());
                    }
                }
                result
            }
            Command::Start => start(),
            Command::Stop => stop(),
            Command::Enable => set_scheduling(true),
            Command::Disable => set_scheduling(false),
            Command::Status => status(),
            Command::Uninstall { data_directory } => uninstall(data_directory.as_deref()),
        }
    }

    fn load_config(path: &Path) -> Result<ControllerConfig, ServiceError> {
        Ok(ControllerConfig::from_toml(&fs::read_to_string(path)?)?)
    }

    fn file_modified(path: &Path) -> Option<SystemTime> {
        path.metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
    }

    fn service_main(_arguments: Vec<OsString>) {
        let path = SERVICE_CONFIG_PATH.get().cloned();
        if let Err(error) = path
            .ok_or(ServiceError::MissingServiceConfig)
            .and_then(|path| run_service(&path))
        {
            let _ = emergency_log(&format!("service failed: {error}"));
        }
    }

    fn run_service(config_path: &Path) -> Result<(), ServiceError> {
        let (control_tx, control_rx) = mpsc::channel();
        let interactive_wake_tx = control_tx.clone();
        let event_handler = move |event| -> ServiceControlHandlerResult {
            match event {
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                ServiceControl::Stop => {
                    let _ = control_tx.send(ControllerCommand::Stop);
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::UserEvent(code) if code.to_raw() == CONTROL_ENABLE => {
                    let _ = control_tx.send(ControllerCommand::Enable);
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::UserEvent(code) if code.to_raw() == CONTROL_DISABLE => {
                    let _ = control_tx.send(ControllerCommand::Disable);
                    ServiceControlHandlerResult::NoError
                }
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        };
        let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)?;
        status_handle.set_service_status(service_status(ServiceState::StartPending, 0))?;

        let install_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
        let log_path = install_dir.join(LOG_FILE_NAME);
        let managed_state_path = install_dir.join(MANAGED_STATE_FILE_NAME);
        let background_state_path = install_dir.join(BACKGROUND_STATE_FILE_NAME);
        let runtime_state_path = install_dir.join(RUNTIME_STATE_FILE_NAME);
        let status_path = install_dir.join(STATUS_FILE_NAME);
        let fail_start = |operation: ServiceError| {
            let result = match cleanup_persisted_state(install_dir) {
                Ok(()) => Err(operation),
                Err(recovery) => Err(ServiceError::TransactionRecovery {
                    operation: Box::new(operation),
                    recovery: recovery.to_string(),
                }),
            };
            let _ = status_handle.set_service_status(service_status(ServiceState::Stopped, 1));
            result
        };
        let config = match load_config(config_path) {
            Ok(config) => config,
            Err(error) => return fail_start(error),
        };
        let tray_binary_path = match std::env::current_exe() {
            Ok(path) => path
                .parent()
                .unwrap_or(install_dir)
                .join("winsched-tray.exe"),
            Err(error) => return fail_start(ServiceError::Io(error)),
        };
        let mut logger = match EventLogger::service(log_path, config.logging) {
            Ok(logger) => logger,
            Err(error) => {
                let detail = format!("service log initialization failed: {error}");
                let _ = emergency_log(&detail);
                return fail_start(ServiceError::Io(error));
            }
        };
        if let Err(error) =
            status_handle.set_service_status(service_status(ServiceState::Running, 0))
        {
            return fail_start(error.into());
        }

        let result = run_controller(
            config,
            ControllerFiles {
                config: Some(config_path),
                managed_state: Some(&managed_state_path),
                background_state: Some(&background_state_path),
                tray_binary: Some(&tray_binary_path),
                runtime_state: Some(&runtime_state_path),
                status: Some(&status_path),
            },
            Some(&control_rx),
            None,
            Some(interactive_wake_tx),
            &mut logger,
        );
        let exit_code = u32::from(result.is_err());
        let status_result = status_handle
            .set_service_status(service_status(ServiceState::Stopped, exit_code))
            .map_err(ServiceError::from);
        match (result, status_result) {
            (Err(error), _) | (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    fn service_status(state: ServiceState, exit_code: u32) -> ServiceStatus {
        ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: state,
            controls_accepted: if state == ServiceState::Running {
                ServiceControlAccept::STOP
            } else {
                ServiceControlAccept::empty()
            },
            exit_code: ServiceExitCode::Win32(exit_code),
            checkpoint: 0,
            wait_hint: Duration::from_secs(5),
            process_id: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn recover_controller_initialization<T>(
        result: Result<T, ServiceError>,
        logger: &mut EventLogger,
        managed: &mut ManagedAssignments,
        managed_state_path: Option<&Path>,
        background: &mut ManagedBackground,
        background_state_path: Option<&Path>,
    ) -> Result<T, ServiceError> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                let placement = cleanup_managed(logger, managed, managed_state_path);
                let efficiency = cleanup_background(logger, background, background_state_path);
                match (placement, efficiency) {
                    (Ok(placement), Ok(efficiency))
                        if placement.failed == 0 && efficiency.failed == 0 =>
                    {
                        Err(error)
                    }
                    (placement, efficiency) => Err(ServiceError::TransactionRecovery {
                        operation: Box::new(error),
                        recovery: format!(
                            "ownership cleanup after initialization failure: placement={placement:?}, background={efficiency:?}"
                        ),
                    }),
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn run_controller(
        mut config: ControllerConfig,
        files: ControllerFiles<'_>,
        control: Option<&mpsc::Receiver<ControllerCommand>>,
        max_iterations: Option<u16>,
        interactive_wake_tx: Option<mpsc::Sender<ControllerCommand>>,
        logger: &mut EventLogger,
    ) -> Result<(), ServiceError> {
        let managed_result = files
            .managed_state
            .map_or_else(|| Ok(BTreeMap::new()), load_managed_state);
        let background_result = files
            .background_state
            .map_or_else(|| Ok(BTreeMap::new()), load_background_state);
        let (mut managed, mut managed_background) = match (managed_result, background_result) {
            (Ok(managed), Ok(background)) => (managed, background),
            (Ok(mut managed), Err(error)) => {
                return match cleanup_managed(logger, &mut managed, files.managed_state) {
                    Ok(report) if report.failed == 0 => Err(error),
                    cleanup => Err(ServiceError::TransactionRecovery {
                        operation: Box::new(error),
                        recovery: format!(
                            "placement cleanup after background journal load failure: {cleanup:?}"
                        ),
                    }),
                };
            }
            (Err(error), Ok(mut background)) => {
                return match cleanup_background(logger, &mut background, files.background_state) {
                    Ok(report) if report.failed == 0 => Err(error),
                    cleanup => Err(ServiceError::TransactionRecovery {
                        operation: Box::new(error),
                        recovery: format!(
                            "background cleanup after placement journal load failure: {cleanup:?}"
                        ),
                    }),
                };
            }
            (Err(error), Err(_)) => return Err(error),
        };
        let mut runtime = recover_controller_initialization(
            files.runtime_state.map_or_else(
                || Ok(RuntimeState::for_controller_mode(config.controller_mode)),
                |path| load_runtime_state(path, config.controller_mode),
            ),
            logger,
            &mut managed,
            files.managed_state,
            &mut managed_background,
            files.background_state,
        )?;
        let topology = recover_controller_initialization(
            platform::system_topology().map_err(ServiceError::from),
            logger,
            &mut managed,
            files.managed_state,
            &mut managed_background,
            files.background_state,
        )?;
        let mut reserve_plan = system_reserve_plan(&topology, &config);
        let mut placement_topology = topology.excluding_reserved_cpu_sets(&reserve_plan);
        let mut sampler = recover_controller_initialization(
            platform::LoadSampler::new(&topology).map_err(ServiceError::from),
            logger,
            &mut managed,
            files.managed_state,
            &mut managed_background,
            files.background_state,
        )?;
        let mut engine = recover_controller_initialization(
            PolicyEngine::new(config.policy).map_err(ServiceError::from),
            logger,
            &mut managed,
            files.managed_state,
            &mut managed_background,
            files.background_state,
        )?;
        let latency_probe = recover_controller_initialization(
            SchedulerLatencyProbe::start(
                config.responsiveness.enabled && config.responsiveness.latency_guard_enabled,
                LATENCY_PROBE_INTERVAL,
                LATENCY_PROBE_WINDOW_SAMPLES,
            )
            .map_err(ServiceError::from),
            logger,
            &mut managed,
            files.managed_state,
            &mut managed_background,
            files.background_state,
        )?;
        let mut width_controller =
            AdaptiveWidthController::new(adaptive_width_config(&config, &placement_topology));
        let interactive_wake_pending = Arc::new(AtomicBool::new(false));
        let interactive_wake_enabled = Arc::new(AtomicBool::new(
            managed_background
                .values()
                .any(|record| !record.ownership.is_empty()),
        ));
        let interactive_wake = interactive_wake_tx.map(|sender| {
            let pending = Arc::clone(&interactive_wake_pending);
            let enabled = Arc::clone(&interactive_wake_enabled);
            Arc::new(move || {
                if enabled.load(Ordering::Acquire) {
                    request_interactive_wake(&sender, &pending);
                }
            }) as platform::InteractiveStateWake
        });
        let interactive_server = files.tray_binary.and_then(|path| {
            match platform::InteractiveStateServer::start(path, interactive_wake) {
                Ok(server) => Some(server),
                Err(error) => {
                    logger.emit(json!({
                        "event": "interactive_probe_server_unavailable",
                        "error": error.to_string(),
                    }));
                    None
                }
            }
        });
        let memory_pressure_monitor = match platform::MemoryPressureMonitor::new() {
            Ok(monitor) => Some(monitor),
            Err(error) => {
                logger.emit(json!({
                    "event": "memory_pressure_monitor_unavailable",
                    "error": error.to_string(),
                }));
                None
            }
        };
        let mut background_clear_streaks = BTreeMap::<ProcessKey, u8>::new();
        let mut previous_cpu_times = BTreeMap::<ProcessKey, u64>::new();
        let mut last_low_memory = None;
        let started = Instant::now();
        let mut last_policy_evaluation = Instant::now();
        let mut iteration = 0u64;
        let mut status_publish_gate = StatusPublishGate::default();
        let mut responsiveness_log_gate = ResponsivenessLogGate::default();
        // Re-read once on the first tick. This closes the narrow startup race where Settings
        // can replace the file after the service loaded it but before its first metadata read.
        let mut config_modified = files.config.map(|_| SystemTime::UNIX_EPOCH);
        let mut status = ControllerStatus::starting(
            std::process::id(),
            runtime.scheduling_enabled,
            &config,
            reserve_plan.clone(),
            topology.llc_domains.len(),
            unix_time_ms(),
        );
        status.phase = if controller_evaluation_active(&config, &runtime) {
            ControllerPhase::Running
        } else {
            ControllerPhase::Disabled
        };
        status.scheduler_latency = latency_probe.status();
        status.memory_profile_physical_cores = width_controller.current_physical_cores();
        status.responsiveness_pressure = width_controller.pressure();
        let loop_result: Result<(), ServiceError> = (|| {
            if !controller_mutations_active(&config, &runtime) {
                let cleanup = cleanup_managed(logger, &mut managed, files.managed_state)?;
                let background_cleanup =
                    cleanup_background(logger, &mut managed_background, files.background_state)?;
                if let Some(error) = combined_cleanup_error(cleanup, background_cleanup) {
                    status.last_error = Some(error);
                }
            }
            status.managed_processes = managed.len();
            status.background_efficiency.managed_processes = managed_background.len();
            publish_controller_status(
                &mut status_publish_gate,
                files.status,
                &mut status,
                0,
                true,
            )?;
            sampler.prime()?;
            logger.emit(json!({
                "event": "controller_started",
                "controller_mode": config.controller_mode,
                "scheduling_enabled": runtime.scheduling_enabled,
                "llc_domains": topology.llc_domains.len(),
                "physical_cores": reserve_plan.physical_core_count,
                "reserved_physical_cores": reserve_plan.reserved_physical_cores,
                "reserved_cpu_sets": reserve_plan.reserved_cpu_set_ids,
                "rules": config.rules.len(),
                "background_efficiency": config.background_efficiency,
            }));

            loop {
                interactive_wake_enabled.store(
                    managed_background
                        .values()
                        .any(|record| !record.ownership.is_empty()),
                    Ordering::Release,
                );
                let interval = controller_wait_interval(
                    &config,
                    &runtime,
                    &managed_background,
                    last_policy_evaluation,
                );
                let command = wait_for_command(control, interval);
                if command == ControllerCommand::Tick {
                    interactive_wake_pending.store(false, Ordering::Release);
                }
                let evaluation_time_ms = controller_elapsed_ms(started);
                match command {
                    ControllerCommand::Stop => {
                        status.phase = ControllerPhase::Stopping;
                        status.scheduler_latency = latency_probe.status();
                        publish_controller_status(
                            &mut status_publish_gate,
                            files.status,
                            &mut status,
                            evaluation_time_ms,
                            true,
                        )?;
                        break Ok(());
                    }
                    ControllerCommand::Enable => {
                        set_runtime_enabled(
                            true,
                            &mut runtime,
                            files.runtime_state,
                            &mut engine,
                            config.policy,
                            &mut previous_cpu_times,
                            logger,
                        )?;
                        status.scheduling_enabled = true;
                        status.phase = ControllerPhase::Running;
                        status.last_activity =
                            Some("Scheduling enabled from tray or CLI".to_owned());
                        status.last_error = None;
                        status.scheduler_latency = latency_probe.status();
                        publish_controller_status(
                            &mut status_publish_gate,
                            files.status,
                            &mut status,
                            evaluation_time_ms,
                            true,
                        )?;
                        continue;
                    }
                    ControllerCommand::Disable => {
                        set_runtime_enabled(
                            false,
                            &mut runtime,
                            files.runtime_state,
                            &mut engine,
                            config.policy,
                            &mut previous_cpu_times,
                            logger,
                        )?;
                        let cleanup = cleanup_managed(logger, &mut managed, files.managed_state)?;
                        let background_cleanup = cleanup_background(
                            logger,
                            &mut managed_background,
                            files.background_state,
                        )?;
                        background_clear_streaks.clear();
                        status.scheduling_enabled = false;
                        status.phase = ControllerPhase::Disabled;
                        status.managed_processes = managed.len();
                        status.background_efficiency.managed_processes = managed_background.len();
                        status.last_activity = Some(
                            if cleanup.failed == 0 && background_cleanup.failed == 0 {
                                "Scheduling disabled; managed assignments and background policies cleared"
                            .to_owned()
                            } else {
                                format!(
                                    "Scheduling disabled; {} CPU assignment(s) and {} background policy(s) await cleanup retry",
                                    cleanup.failed, background_cleanup.failed
                                )
                            },
                        );
                        status.last_error = combined_cleanup_error(cleanup, background_cleanup);
                        status.scheduler_latency = latency_probe.status();
                        publish_controller_status(
                            &mut status_publish_gate,
                            files.status,
                            &mut status,
                            evaluation_time_ms,
                            true,
                        )?;
                        continue;
                    }
                    ControllerCommand::Tick => {}
                }
                let reload = reload_config_if_changed(
                    files.config,
                    &mut config_modified,
                    &mut config,
                    &mut engine,
                    &mut managed,
                    files.managed_state,
                    logger,
                )?;
                if !matches!(&reload, ConfigReload::Unchanged) {
                    reserve_plan = system_reserve_plan(&topology, &config);
                    placement_topology = topology.excluding_reserved_cpu_sets(&reserve_plan);
                    latency_probe.set_enabled(
                        config.responsiveness.enabled
                            && config.responsiveness.latency_guard_enabled,
                    );
                    width_controller
                        .reconfigure(adaptive_width_config(&config, &placement_topology));
                }
                status.scheduler_latency = latency_probe.status();
                status.memory_profile_physical_cores = width_controller.current_physical_cores();
                status.responsiveness_pressure = width_controller.pressure();
                if let Some(event) =
                    apply_reload_status(&mut status, &config, &reserve_plan, reload)
                {
                    // This status receipt is authoritative for Settings and must reach disk before
                    // any optional event-log write or rotation can fail.
                    publish_controller_status(
                        &mut status_publish_gate,
                        files.status,
                        &mut status,
                        evaluation_time_ms,
                        true,
                    )?;
                    logger.emit(event);
                }
                if !controller_evaluation_active(&config, &runtime) {
                    let prior_error = status.last_error.clone();
                    let cleanup = cleanup_managed(logger, &mut managed, files.managed_state)?;
                    let background_cleanup = cleanup_background(
                        logger,
                        &mut managed_background,
                        files.background_state,
                    )?;
                    background_clear_streaks.clear();
                    status.phase = ControllerPhase::Disabled;
                    status.managed_processes = managed.len();
                    status.background_efficiency.managed_processes = managed_background.len();
                    status.last_error = combined_cleanup_error(cleanup, background_cleanup);
                    iteration = iteration.saturating_add(1);
                    status.iteration = iteration;
                    let important_status_change = status.last_error != prior_error;
                    publish_controller_status(
                        &mut status_publish_gate,
                        files.status,
                        &mut status,
                        evaluation_time_ms,
                        important_status_change,
                    )?;
                    if max_iterations.is_some_and(|limit| iteration >= u64::from(limit)) {
                        break Ok(());
                    }
                    continue;
                }
                let policy_elapsed = last_policy_evaluation.elapsed();
                let configured_policy_interval = Duration::from_millis(config.sample_interval_ms);
                if policy_elapsed < configured_policy_interval {
                    let status_error_before_guard = status.last_error.clone();
                    let tick = run_background_safety_tick(
                        &config,
                        &runtime,
                        &topology,
                        interactive_server.as_ref(),
                        memory_pressure_monitor.as_ref(),
                        &mut last_low_memory,
                        &mut previous_cpu_times,
                        &mut background_clear_streaks,
                        &mut managed,
                        files.managed_state,
                        &mut managed_background,
                        files.background_state,
                        &mut status,
                        logger,
                    )?;
                    status.managed_processes = managed.len();
                    status.phase = ControllerPhase::Running;
                    status.scheduler_latency = latency_probe.status();
                    let important_status_change = status.last_error != status_error_before_guard
                        || status.background_efficiency != tick.prior_status;
                    publish_controller_status(
                        &mut status_publish_gate,
                        files.status,
                        &mut status,
                        evaluation_time_ms,
                        important_status_change,
                    )?;
                    continue;
                }
                last_policy_evaluation = Instant::now();
                let effective_sample_interval_ms =
                    u64::try_from(policy_elapsed.as_millis()).unwrap_or(u64::MAX);
                let loads = match sampler.sample() {
                    Ok(loads) => loads,
                    Err(error) => {
                        let detail = format!("Load telemetry sample skipped: {error}");
                        status.last_activity = Some("Load telemetry sample skipped".to_owned());
                        status.last_error = Some(detail.clone());
                        status.scheduler_latency = latency_probe.status();
                        logger.emit(json!({
                            "event": "load_sample_skipped",
                            "error": error.to_string(),
                        }));
                        if let Ok(mut replacement) = platform::LoadSampler::new(&topology)
                            && replacement.prime().is_ok()
                        {
                            sampler = replacement;
                        }
                        let _ = run_background_safety_tick(
                            &config,
                            &runtime,
                            &topology,
                            interactive_server.as_ref(),
                            memory_pressure_monitor.as_ref(),
                            &mut last_low_memory,
                            &mut previous_cpu_times,
                            &mut background_clear_streaks,
                            &mut managed,
                            files.managed_state,
                            &mut managed_background,
                            files.background_state,
                            &mut status,
                            logger,
                        )?;
                        iteration = iteration.saturating_add(1);
                        status.iteration = iteration;
                        publish_controller_status(
                            &mut status_publish_gate,
                            files.status,
                            &mut status,
                            evaluation_time_ms,
                            true,
                        )?;
                        if max_iterations.is_some_and(|limit| iteration >= u64::from(limit)) {
                            break Ok(());
                        }
                        continue;
                    }
                };
                let status_error_before_evaluation = status.last_error.clone();
                if status
                    .last_error
                    .as_deref()
                    .is_some_and(|error| error.starts_with("Load telemetry sample skipped:"))
                {
                    status.last_error = None;
                }
                status.maximum_dpc_time_bps = loads
                    .iter()
                    .map(|load| load.dpc_time_bps)
                    .max()
                    .unwrap_or(0);
                status.maximum_interrupt_time_bps = loads
                    .iter()
                    .map(|load| load.interrupt_time_bps)
                    .max()
                    .unwrap_or(0);
                let adjustment = width_controller.evaluate(
                    evaluation_time_ms,
                    status.scheduler_latency,
                    status.maximum_dpc_time_bps,
                    status.maximum_interrupt_time_bps,
                );
                if let Some(adjustment) = adjustment {
                    let detail = width_adjustment_summary(adjustment);
                    status.last_responsiveness_adjustment = Some(detail.clone());
                    logger.emit(json!({
                        "event": "memory_profile_width_changed",
                        "adjustment": detail,
                        "pressure": width_controller.pressure(),
                        "scheduler_latency": status.scheduler_latency,
                        "maximum_dpc_time_bps": status.maximum_dpc_time_bps,
                        "maximum_interrupt_time_bps": status.maximum_interrupt_time_bps,
                    }));
                }
                status.memory_profile_physical_cores = width_controller.current_physical_cores();
                status.responsiveness_pressure = width_controller.pressure();
                let responsiveness_signature = ResponsivenessSignature {
                    pressure: status.responsiveness_pressure,
                    memory_profile_physical_cores: status.memory_profile_physical_cores,
                };
                if let Some(reason) = responsiveness_log_gate.decide(
                    evaluation_time_ms,
                    responsiveness_signature,
                    adjustment.is_some(),
                ) {
                    logger.emit(json!({
                        "event": "responsiveness_sample",
                        "reason": reason.as_str(),
                        "scheduler_latency": status.scheduler_latency,
                        "maximum_dpc_time_bps": status.maximum_dpc_time_bps,
                        "maximum_interrupt_time_bps": status.maximum_interrupt_time_bps,
                        "memory_profile_physical_cores": status.memory_profile_physical_cores,
                        "pressure": status.responsiveness_pressure,
                        "domain_loads": loads,
                    }));
                }
                let background_tick = run_background_safety_tick(
                    &config,
                    &runtime,
                    &topology,
                    interactive_server.as_ref(),
                    memory_pressure_monitor.as_ref(),
                    &mut last_low_memory,
                    &mut previous_cpu_times,
                    &mut background_clear_streaks,
                    &mut managed,
                    files.managed_state,
                    &mut managed_background,
                    files.background_state,
                    &mut status,
                    logger,
                )?;
                let processes = background_tick.processes;
                let prior_background_status = background_tick.prior_status;
                let background_error = background_tick.error;

                let observations = build_ranked_observations(
                    &config,
                    &managed,
                    &processes,
                    &placement_topology,
                    width_controller.current_physical_cores(),
                    &mut previous_cpu_times,
                    effective_sample_interval_ms,
                );

                let decisions = match engine.evaluate(
                    evaluation_time_ms,
                    &placement_topology,
                    &loads,
                    &observations,
                ) {
                    Ok(decisions) => decisions,
                    Err(error) => break Err(error.into()),
                };
                for decision in decisions {
                    log_decision(logger, &processes, &decision);
                    status.last_activity = Some(decision_summary(&processes, &decision));
                    if decision.enforce && decision.action.is_mutation() {
                        if let Some(error) = enforce_decision(
                            logger,
                            &mut engine,
                            &mut managed,
                            files.managed_state,
                            &decision,
                            evaluation_time_ms,
                        )? {
                            status.last_error = Some(error);
                        } else {
                            status.last_error = None;
                        }
                    }
                }
                if let Some(error) = background_error {
                    status.last_error = Some(format!("Background efficiency: {error}"));
                }

                iteration = iteration.saturating_add(1);
                status.iteration = iteration;
                status.managed_processes = managed.len();
                status.phase = ControllerPhase::Running;
                status.scheduler_latency = latency_probe.status();
                let important_status_change = adjustment.is_some()
                    || status.last_error != status_error_before_evaluation
                    || status.background_efficiency != prior_background_status;
                publish_controller_status(
                    &mut status_publish_gate,
                    files.status,
                    &mut status,
                    evaluation_time_ms,
                    important_status_change,
                )?;
                if max_iterations.is_some_and(|limit| iteration >= u64::from(limit)) {
                    break Ok(());
                }
            }
        })();

        let placement_cleanup = cleanup_managed(logger, &mut managed, files.managed_state);
        let background_cleanup =
            cleanup_background(logger, &mut managed_background, files.background_state);
        let cleanup_result = match (placement_cleanup, background_cleanup) {
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
            (Ok(placement), Ok(background)) if placement.failed != 0 || background.failed != 0 => {
                Err(ServiceError::CleanupIncomplete(
                    placement.failed.saturating_add(background.failed),
                ))
            }
            (Ok(_), Ok(_)) => Ok(()),
        };
        status.phase = if loop_result.is_ok() && cleanup_result.is_ok() {
            ControllerPhase::Stopped
        } else {
            ControllerPhase::Error
        };
        status.managed_processes = managed.len();
        status.background_efficiency.managed_processes = managed_background.len();
        status.scheduler_latency = latency_probe.status();
        status.memory_profile_physical_cores = width_controller.current_physical_cores();
        status.responsiveness_pressure = width_controller.pressure();
        if let Err(error) = &loop_result {
            status.last_error = Some(error.to_string());
        } else if let Err(error) = &cleanup_result {
            status.last_error = Some(error.to_string());
        }
        let status_result = publish_controller_status(
            &mut status_publish_gate,
            files.status,
            &mut status,
            controller_elapsed_ms(started),
            true,
        );
        logger.emit(json!({
            "event": "controller_stopped",
            "success": loop_result.is_ok(),
        }));
        match (loop_result, cleanup_result, status_result) {
            (Err(error), _, _) | (Ok(()), Err(error), _) | (Ok(()), Ok(()), Err(error)) => {
                Err(error)
            }
            (Ok(()), Ok(()), Ok(())) => Ok(()),
        }
    }

    fn system_reserve_plan(topology: &Topology, config: &ControllerConfig) -> SystemReservePlan {
        if !config.responsiveness.enabled {
            return topology.plan_system_reserve(0, 0, 0);
        }
        topology.plan_system_reserve(
            config.responsiveness.system_reserve_percent,
            config.responsiveness.minimum_reserved_cores,
            config.responsiveness.maximum_reserved_cores,
        )
    }

    fn controller_wait_interval(
        config: &ControllerConfig,
        runtime: &RuntimeState,
        managed_background: &ManagedBackground,
        last_policy_evaluation: Instant,
    ) -> Duration {
        let configured = Duration::from_millis(config.sample_interval_ms);
        let until_policy = configured.saturating_sub(last_policy_evaluation.elapsed());
        let background_rule_active = controller_mutations_active(config, runtime)
            && config
                .rules
                .iter()
                .any(|rule| config.background_efficiency_applies(&rule.image));
        if background_rule_active || !managed_background.is_empty() {
            until_policy.min(BACKGROUND_SAFETY_INTERVAL)
        } else {
            until_policy
        }
    }

    const fn controller_evaluation_active(
        config: &ControllerConfig,
        runtime: &RuntimeState,
    ) -> bool {
        match config.controller_mode {
            ControllerMode::Off => false,
            ControllerMode::Observe => true,
            ControllerMode::Auto => runtime.scheduling_enabled,
        }
    }

    const fn controller_mutations_active(
        config: &ControllerConfig,
        runtime: &RuntimeState,
    ) -> bool {
        matches!(config.controller_mode, ControllerMode::Auto) && runtime.scheduling_enabled
    }

    fn adaptive_width_config(
        config: &ControllerConfig,
        placement_topology: &Topology,
    ) -> AdaptiveWidthConfig {
        let available =
            u16::try_from(placement_topology.assignable_physical_core_count()).unwrap_or(u16::MAX);
        let maximum_physical_cores = config
            .responsiveness
            .memory
            .maximum_physical_cores
            .min(available);
        let minimum_physical_cores = config
            .responsiveness
            .memory
            .minimum_physical_cores
            .min(maximum_physical_cores);
        AdaptiveWidthConfig {
            enabled: config.responsiveness.enabled && config.responsiveness.latency_guard_enabled,
            minimum_physical_cores,
            maximum_physical_cores,
            latency_target_p99_us: config.responsiveness.latency_target_p99_us,
            latency_recovery_p99_us: config.responsiveness.latency_recovery_p99_us,
            stability_samples: config.responsiveness.adjustment_stability_samples,
            resize_cooldown_ms: config.responsiveness.memory.resize_cooldown_ms,
        }
    }

    fn width_adjustment_summary(adjustment: WidthAdjustment) -> String {
        match adjustment {
            WidthAdjustment::Shrunk { from, to } => {
                format!("Memory profile reduced from {from} to {to} physical cores")
            }
            WidthAdjustment::Expanded { from, to } => {
                format!("Memory profile expanded from {from} to {to} physical cores")
            }
        }
    }

    fn expected_domain_cpu_sets(
        topology: &Topology,
        domain: winsched_core::LlcDomainKey,
        placement: PlacementMode,
    ) -> Vec<u32> {
        let preference = match placement {
            PlacementMode::Performance => ProcessorClassPreference::Fastest,
            PlacementMode::Efficiency => ProcessorClassPreference::MostEfficient,
            PlacementMode::Off
            | PlacementMode::Sticky
            | PlacementMode::Auto
            | PlacementMode::Strict(_) => ProcessorClassPreference::Any,
        };
        topology
            .llc_domains
            .iter()
            .find(|candidate| candidate.key == domain)
            .map_or_else(Vec::new, |candidate| {
                candidate.cpu_set_ids_for_class(preference)
            })
    }

    fn build_ranked_observations(
        config: &ControllerConfig,
        managed: &ManagedAssignments,
        processes: &[platform::ObservedProcess],
        placement_topology: &Topology,
        memory_profile_physical_cores: u16,
        previous_cpu_times: &mut BTreeMap<ProcessKey, u64>,
        sample_interval_ms: u64,
    ) -> Vec<winsched_core::adaptive::ProcessObservation> {
        let mut ranked = Vec::new();
        for process in processes {
            let cpu_delta = previous_cpu_times
                .insert(process.key, process.cpu_time_100ns)
                .map_or(0, |previous| {
                    process.cpu_time_100ns.saturating_sub(previous)
                });
            let recovered_placement = managed.get(&process.key);
            let recovered = recovered_placement.is_some();
            let explicit_rule = config
                .rules
                .iter()
                .any(|rule| rule.image.eq_ignore_ascii_case(&process.image_name));
            let rule = config.resolve(&process.image_name);
            let (mut placement, enforcement, profile) =
                match (recovered, config.controller_mode, rule) {
                    (true, mode, _) if mode != ControllerMode::Auto => (
                        PlacementMode::Off,
                        winsched_core::adaptive::EnforcementMode::Apply,
                        WorkloadProfile::Balanced,
                    ),
                    (_, _, Some(rule)) => (rule.placement, rule.enforcement, rule.profile),
                    (true, _, None) => (
                        PlacementMode::Off,
                        winsched_core::adaptive::EnforcementMode::Apply,
                        WorkloadProfile::Balanced,
                    ),
                    (false, _, None) => continue,
                };
            if profile == WorkloadProfile::Interactive && placement == PlacementMode::Auto {
                placement = PlacementMode::Sticky;
            }
            if !recovered
                && (!explicit_rule
                    && process_utilization_bps(cpu_delta, sample_interval_ms)
                        < u32::from(config.minimum_process_utilization_bps)
                    || process.exclusion.is_some())
            {
                continue;
            }
            let mut observation = process.policy_observation(placement, enforcement);
            let preferred_partition = match profile {
                WorkloadProfile::Memory
                    if !matches!(placement, PlacementMode::Off | PlacementMode::Strict(_)) =>
                {
                    placement_topology.plan_spread_partition(
                        usize::from(memory_profile_physical_cores),
                        config.responsiveness.memory.use_smt,
                    )
                }
                WorkloadProfile::Compute
                    if !matches!(placement, PlacementMode::Off | PlacementMode::Strict(_)) =>
                {
                    placement_topology.plan_spread_partition(usize::MAX, true)
                }
                WorkloadProfile::Interactive
                | WorkloadProfile::Memory
                | WorkloadProfile::Compute
                | WorkloadProfile::Background
                | WorkloadProfile::Balanced => None,
            };
            observation.preferred_partition = preferred_partition;
            if let Some(recovered_placement) = recovered_placement {
                observation.assignment_origin = AssignmentOrigin::Managed;
                observation.current_domain = Some(recovered_placement.anchor_domain);
                if let Some(partition) = &observation.preferred_partition {
                    observation.refresh_required =
                        partition.cpu_set_ids != process.default_cpu_set_ids;
                } else {
                    let expected = expected_domain_cpu_sets(
                        placement_topology,
                        recovered_placement.anchor_domain,
                        placement,
                    );
                    if expected.is_empty() {
                        observation.current_domain = None;
                    } else {
                        observation.refresh_required = expected != process.default_cpu_set_ids;
                    }
                }
            }
            ranked.push((observation, cpu_delta));
        }
        ranked.sort_by_key(|(observation, cpu_delta)| (Reverse(*cpu_delta), observation.key));
        ranked
            .into_iter()
            .map(|(observation, _)| observation)
            .collect()
    }

    fn process_utilization_bps(cpu_delta_100ns: u64, sample_interval_ms: u64) -> u32 {
        let interval_100ns = u128::from(sample_interval_ms).saturating_mul(10_000);
        if interval_100ns == 0 {
            return 0;
        }
        let basis_points = u128::from(cpu_delta_100ns).saturating_mul(10_000) / interval_100ns;
        u32::try_from(basis_points).unwrap_or(u32::MAX)
    }

    fn reconcile_managed(
        logger: &mut EventLogger,
        managed: &mut ManagedAssignments,
        state_path: Option<&Path>,
        processes: &[platform::ObservedProcess],
    ) -> Result<(), ServiceError> {
        let entries = managed
            .iter()
            .map(|(key, placement)| (*key, placement.clone()))
            .collect::<Vec<_>>();
        let mut changed = false;
        for (key, placement) in entries {
            let Some(process) = processes.iter().find(|process| process.key == key) else {
                managed.remove(&key);
                changed = true;
                continue;
            };
            if placement.cpu_set_ids.is_empty() {
                if process.current_domain == Some(placement.anchor_domain)
                    && !process.default_cpu_set_ids.is_empty()
                {
                    if let Some(current) = managed.get_mut(&key) {
                        current.cpu_set_ids.clone_from(&process.default_cpu_set_ids);
                        changed = true;
                    }
                } else {
                    managed.remove(&key);
                    changed = true;
                    continue;
                }
            } else if process.default_cpu_set_ids != placement.cpu_set_ids {
                managed.remove(&key);
                changed = true;
                continue;
            }
            let Some(exclusion) = process.exclusion else {
                continue;
            };
            let result = platform::clear_process_key(key);
            let target_gone = result
                .as_ref()
                .is_err_and(winsched::platform::PlatformError::process_no_longer_matches);
            logger.emit(json!({
                "event": "cleanup_excluded",
                "process": key,
                "exclusion": exclusion,
                "succeeded": result.is_ok() || target_gone,
                "target_gone_or_reused": target_gone,
                "error": result.as_ref().err().map(ToString::to_string),
            }));
            if result.is_ok() || target_gone {
                managed.remove(&key);
                changed = true;
            }
        }
        if changed {
            persist_managed_state(state_path, managed)?;
        }
        Ok(())
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum ConfigReload {
        Unchanged,
        Reloaded,
        Rejected { error: String, fail_closed: bool },
    }

    fn apply_reload_status(
        status: &mut ControllerStatus,
        config: &ControllerConfig,
        reserve_plan: &SystemReservePlan,
        reload: ConfigReload,
    ) -> Option<Value> {
        status.configured_mode = config.controller_mode;
        status.applied_config_fingerprint = config.fingerprint();
        status.applied_logging = config.logging;
        status.applied_background_efficiency = config.background_efficiency;
        status.applied_responsiveness = config.responsiveness;
        status.system_reserve.clone_from(reserve_plan);
        match reload {
            ConfigReload::Unchanged => None,
            ConfigReload::Reloaded => {
                status.config_reload_sequence = status.config_reload_sequence.saturating_add(1);
                status.config_reload_result = ConfigReloadResult::Reloaded;
                status.config_reload_error = None;
                status.last_activity = Some("Configuration reloaded".to_owned());
                status.last_error = None;
                Some(json!({
                    "event": "config_reloaded",
                    "controller_mode": config.controller_mode,
                    "logging": config.logging,
                    "background_efficiency": config.background_efficiency,
                    "responsiveness": config.responsiveness,
                    "reserved_physical_cores": reserve_plan.reserved_physical_cores.len(),
                    "reserved_cpu_sets": reserve_plan.reserved_cpu_set_ids.len(),
                    "rules": config.rules.len(),
                }))
            }
            ConfigReload::Rejected { error, fail_closed } => {
                status.config_reload_sequence = status.config_reload_sequence.saturating_add(1);
                status.config_reload_result = ConfigReloadResult::Rejected;
                status.config_reload_error = Some(error.clone());
                status.last_activity = Some(if fail_closed {
                    "Configuration rejected; fail-closed".to_owned()
                } else {
                    "Configuration rejected; prior configuration retained".to_owned()
                });
                status.last_error = Some(error.clone());
                Some(json!({
                    "event": if fail_closed {
                        "config_rejected_fail_closed"
                    } else {
                        "config_rejected"
                    },
                    "error": error,
                }))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn reload_config_if_changed(
        config_path: Option<&Path>,
        previous_modified: &mut Option<SystemTime>,
        config: &mut ControllerConfig,
        engine: &mut PolicyEngine,
        managed: &mut ManagedAssignments,
        state_path: Option<&Path>,
        logger: &mut EventLogger,
    ) -> Result<ConfigReload, ServiceError> {
        let Some(path) = config_path else {
            return Ok(ConfigReload::Unchanged);
        };
        let modified = file_modified(path);
        if modified == *previous_modified {
            return Ok(ConfigReload::Unchanged);
        }

        let result = match load_config(path) {
            Ok(updated) => match PolicyEngine::new(updated.policy) {
                Err(error) => ConfigReload::Rejected {
                    error: error.to_string(),
                    fail_closed: false,
                },
                Ok(updated_engine) => {
                    if let Err(error) = logger.reconfigure(updated.logging) {
                        ConfigReload::Rejected {
                            error: format!(
                                "failed to apply logging configuration; prior configuration retained: {error}"
                            ),
                            fail_closed: false,
                        }
                    } else {
                        *config = updated;
                        *engine = updated_engine;
                        ConfigReload::Reloaded
                    }
                }
            },
            Err(error) => {
                let prior_logging = config.logging;
                let cleanup = cleanup_managed(logger, managed, state_path);
                *config = ControllerConfig {
                    logging: prior_logging,
                    ..ControllerConfig::default()
                };
                *engine = PolicyEngine::new(config.policy)?;
                let detail = match cleanup {
                    Ok(cleanup) if cleanup.failed == 0 => error.to_string(),
                    Ok(cleanup) => format!(
                        "{error}; {} managed assignment(s) await cleanup retry",
                        cleanup.failed
                    ),
                    Err(cleanup_error) => {
                        format!("{error}; managed-assignment cleanup failed: {cleanup_error}")
                    }
                };
                ConfigReload::Rejected {
                    error: detail,
                    fail_closed: true,
                }
            }
        };
        *previous_modified = modified;
        Ok(result)
    }

    fn request_interactive_wake(sender: &mpsc::Sender<ControllerCommand>, pending: &AtomicBool) {
        if pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            && sender.send(ControllerCommand::Tick).is_err()
        {
            pending.store(false, Ordering::Release);
        }
    }

    fn wait_for_command(
        control: Option<&mpsc::Receiver<ControllerCommand>>,
        interval: Duration,
    ) -> ControllerCommand {
        control.map_or_else(
            || {
                std::thread::sleep(interval);
                ControllerCommand::Tick
            },
            |receiver| match receiver.recv_timeout(interval) {
                Ok(ControllerCommand::Tick) => {
                    while let Ok(command) = receiver.try_recv() {
                        if command != ControllerCommand::Tick {
                            return command;
                        }
                    }
                    ControllerCommand::Tick
                }
                Ok(command) => command,
                Err(mpsc::RecvTimeoutError::Disconnected) => ControllerCommand::Stop,
                Err(mpsc::RecvTimeoutError::Timeout) => ControllerCommand::Tick,
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn set_runtime_enabled(
        enabled: bool,
        runtime: &mut RuntimeState,
        runtime_state_path: Option<&Path>,
        engine: &mut PolicyEngine,
        policy: winsched_core::adaptive::PolicyConfig,
        previous_cpu_times: &mut BTreeMap<ProcessKey, u64>,
        logger: &mut EventLogger,
    ) -> Result<(), ServiceError> {
        if runtime.scheduling_enabled == enabled {
            return Ok(());
        }
        let updated = RuntimeState {
            schema_version: RUNTIME_SCHEMA_VERSION,
            scheduling_enabled: enabled,
        };
        persist_runtime_state(runtime_state_path, &updated)?;
        *runtime = updated;
        *engine = PolicyEngine::new(policy)?;
        previous_cpu_times.clear();
        logger.emit(json!({
            "event": "scheduling_changed",
            "scheduling_enabled": enabled,
        }));
        Ok(())
    }

    fn log_decision(
        logger: &mut EventLogger,
        processes: &[platform::ObservedProcess],
        decision: &PolicyDecision,
    ) {
        let image = processes
            .iter()
            .find(|process| process.key == decision.process)
            .map(|process| process.image_name.as_str());
        logger.emit(json!({
            "event": "decision",
            "process": decision.process,
            "image": image,
            "enforce": decision.enforce,
            "action": decision.action,
            "reason": decision.reason,
        }));
    }

    fn decision_summary(
        processes: &[platform::ObservedProcess],
        decision: &PolicyDecision,
    ) -> String {
        let image = processes
            .iter()
            .find(|process| process.key == decision.process)
            .map_or("unknown", |process| process.image_name.as_str());
        let action = match &decision.action {
            PolicyAction::Ignore => "ignored".to_owned(),
            PolicyAction::Keep { domain } => domain.as_ref().map_or_else(
                || "kept unassigned".to_owned(),
                |domain| {
                    format!(
                        "kept on LLC {}:{}",
                        domain.group, domain.last_level_cache_index
                    )
                },
            ),
            PolicyAction::Assign {
                target,
                cpu_set_ids,
            } => format!(
                "assigned partition anchored at LLC {}:{} ({} CPU Sets)",
                target.group,
                target.last_level_cache_index,
                cpu_set_ids.len(),
            ),
            PolicyAction::Move {
                source,
                target,
                cpu_set_ids,
            } => format!(
                "moved LLC {}:{} -> partition anchored at {}:{} ({} CPU Sets)",
                source.group,
                source.last_level_cache_index,
                target.group,
                target.last_level_cache_index,
                cpu_set_ids.len(),
            ),
            PolicyAction::Clear { source } => format!(
                "cleared LLC {}:{} assignment",
                source.group, source.last_level_cache_index
            ),
        };
        format!(
            "{image} (PID {}): {action}; {}",
            decision.process.pid,
            reason_summary(decision.reason)
        )
    }

    const fn reason_summary(reason: DecisionReason) -> &'static str {
        match reason {
            DecisionReason::ModeOff => "mode off",
            DecisionReason::Excluded(_) => "fixed safety exclusion",
            DecisionReason::ExternalAssignment => "external assignment preserved",
            DecisionReason::PendingMutation => "mutation awaiting acknowledgement",
            DecisionReason::PartitionRefresh => "refreshing reserved CPU partition",
            DecisionReason::ProfilePartition => "workload profile partition",
            DecisionReason::ProfilePartitionStable => "workload profile partition stable",
            DecisionReason::InitialPlacement => "initial placement",
            DecisionReason::StickyPlacement => "sticky placement",
            DecisionReason::BelowOverloadThreshold => "load below overload threshold",
            DecisionReason::StabilityWindow => "waiting for stability window",
            DecisionReason::MinimumResidency => "minimum residency",
            DecisionReason::Cooldown => "move cooldown",
            DecisionReason::InsufficientImprovement => "insufficient improvement",
            DecisionReason::NoAlternativeDomain => "no alternative LLC",
            DecisionReason::BetterDomain => "better LLC available",
            DecisionReason::StrictPlacement => "strict placement",
            DecisionReason::AlreadyStrict => "already strictly placed",
            DecisionReason::RateLimited => "mutation rate limit",
        }
    }

    fn enforce_decision(
        logger: &mut EventLogger,
        engine: &mut PolicyEngine,
        managed: &mut ManagedAssignments,
        state_path: Option<&Path>,
        decision: &PolicyDecision,
        evaluation_time_ms: u64,
    ) -> Result<Option<String>, ServiceError> {
        let target = match &decision.action {
            PolicyAction::Assign {
                target,
                cpu_set_ids,
            }
            | PolicyAction::Move {
                target,
                cpu_set_ids,
                ..
            } => {
                let result = platform::apply_process_key(decision.process, cpu_set_ids);
                finish_enforcement(
                    logger,
                    engine,
                    managed,
                    state_path,
                    decision,
                    Some(*target),
                    &result,
                    evaluation_time_ms,
                )?;
                return Ok(result.err().map(|error| error.to_string()));
            }
            PolicyAction::Clear { .. } => None,
            PolicyAction::Ignore | PolicyAction::Keep { .. } => return Ok(None),
        };
        let result = platform::clear_process_key(decision.process);
        finish_enforcement(
            logger,
            engine,
            managed,
            state_path,
            decision,
            target,
            &result,
            evaluation_time_ms,
        )?;
        Ok(result.err().map(|error| error.to_string()))
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_enforcement(
        logger: &mut EventLogger,
        engine: &mut PolicyEngine,
        managed: &mut ManagedAssignments,
        state_path: Option<&Path>,
        decision: &PolicyDecision,
        target: Option<winsched_core::LlcDomainKey>,
        result: &Result<MutationReport, platform::PlatformError>,
        evaluation_time_ms: u64,
    ) -> Result<(), ServiceError> {
        let succeeded = result.is_ok();
        let acknowledged =
            engine.acknowledge(decision.process, target, succeeded, evaluation_time_ms);
        if succeeded {
            if let Some(target) = target {
                let cpu_set_ids = result
                    .as_ref()
                    .map_or_else(|_| Vec::new(), |report| report.observed_cpu_set_ids.clone());
                managed.insert(
                    decision.process,
                    ManagedPlacement {
                        anchor_domain: target,
                        cpu_set_ids,
                    },
                );
            } else {
                managed.remove(&decision.process);
            }
            if let Err(error) = persist_managed_state(state_path, managed) {
                if target.is_some() {
                    let _ = platform::clear_process_key(decision.process);
                    managed.remove(&decision.process);
                }
                return Err(error);
            }
        }
        logger.emit(json!({
            "event": "enforcement",
            "process": decision.process,
            "target": target,
            "succeeded": succeeded,
            "acknowledged": acknowledged,
            "report": result.as_ref().ok(),
            "error": result.as_ref().err().map(ToString::to_string),
        }));
        Ok(())
    }

    fn cleanup_managed(
        logger: &mut EventLogger,
        managed: &mut ManagedAssignments,
        state_path: Option<&Path>,
    ) -> Result<CleanupReport, ServiceError> {
        cleanup_managed_with(logger, managed, state_path, |process| {
            match platform::clear_process_key(process) {
                Ok(_) => Ok(()),
                Err(error) if error.process_no_longer_matches() => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        })
    }

    fn cleanup_managed_with<F>(
        logger: &mut EventLogger,
        managed: &mut ManagedAssignments,
        state_path: Option<&Path>,
        mut clear: F,
    ) -> Result<CleanupReport, ServiceError>
    where
        F: FnMut(ProcessKey) -> Result<(), String>,
    {
        let entries = managed.keys().copied().collect::<Vec<_>>();
        let mut report = CleanupReport {
            attempted: entries.len(),
            ..CleanupReport::default()
        };
        let mut events = Vec::with_capacity(entries.len());
        for process in entries {
            let result = clear(process);
            if result.is_ok() {
                managed.remove(&process);
                report.cleared += 1;
            } else {
                report.failed += 1;
            }
            events.push(json!({
                "event": "cleanup",
                "process": process,
                "succeeded": result.is_ok(),
                "error": result.err(),
            }));
        }

        // Persist first so the ownership journal remains conservative even if optional event
        // logging subsequently fails.
        persist_managed_state(state_path, managed)?;
        for event in events {
            logger.emit(event);
        }
        Ok(report)
    }

    fn combined_cleanup_error(
        placement: CleanupReport,
        background: CleanupReport,
    ) -> Option<String> {
        (placement.failed != 0 || background.failed != 0).then(|| {
            format!(
                "failed to restore {} CPU Set assignment(s) and {} background policy state(s); retry pending",
                placement.failed, background.failed
            )
        })
    }

    const BACKGROUND_CLEAR_STABILITY_SAMPLES: u8 = 2;

    struct BackgroundReconcileReport {
        status: BackgroundEfficiencyStatus,
        last_error: Option<String>,
    }

    struct BackgroundSafetyTick {
        processes: Vec<platform::ObservedProcess>,
        prior_status: BackgroundEfficiencyStatus,
        error: Option<String>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum BackgroundProtection {
        ProbeUnavailable,
        Foreground,
        Visible,
        Audio,
    }

    #[derive(Default)]
    struct SessionActivity {
        publishers: usize,
        window_probe_available: bool,
        audio_probe_available: bool,
        foreground_pids: BTreeSet<u32>,
        visible_pids: BTreeSet<u32>,
        audible_pids: BTreeSet<u32>,
    }

    fn interactive_sessions(
        states: &[InteractiveActivityState],
        now_unix_ms: u64,
    ) -> BTreeMap<u32, SessionActivity> {
        let mut sessions = BTreeMap::<u32, SessionActivity>::new();
        for state in states
            .iter()
            .filter(|state| state.session_id != 0 && state.is_fresh_at(now_unix_ms))
        {
            let session = sessions
                .entry(state.session_id)
                .or_insert_with(|| SessionActivity {
                    window_probe_available: true,
                    audio_probe_available: true,
                    ..SessionActivity::default()
                });
            session.publishers = session.publishers.saturating_add(1);
            session.window_probe_available &= state.window_probe_available;
            session.audio_probe_available &= state.audio_probe_available;
            session.foreground_pids.extend(state.foreground_pid);
            session
                .visible_pids
                .extend(state.visible_pids.iter().copied());
            session
                .audible_pids
                .extend(state.audible_pids.iter().copied());
        }
        sessions
    }

    fn expand_protected_descendants(
        processes: &[platform::ObservedProcess],
        session_id: u32,
        protected: &mut BTreeSet<u32>,
    ) {
        loop {
            let mut changed = false;
            for process in processes
                .iter()
                .filter(|process| process.session_id == Some(session_id))
            {
                if protected.contains(&process.parent_pid) && protected.insert(process.key.pid) {
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn background_protection(
        config: &ControllerConfig,
        process: &platform::ObservedProcess,
        processes: &[platform::ObservedProcess],
        sessions: &BTreeMap<u32, SessionActivity>,
    ) -> Option<BackgroundProtection> {
        let session_id = process.session_id?;
        let Some(session) = sessions.get(&session_id) else {
            return Some(BackgroundProtection::ProbeUnavailable);
        };
        if (config.background_efficiency.protect_foreground
            || config.background_efficiency.protect_visible)
            && !session.window_probe_available
            || config.background_efficiency.protect_audio && !session.audio_probe_available
        {
            return Some(BackgroundProtection::ProbeUnavailable);
        }

        let mut foreground = session.foreground_pids.clone();
        let mut visible = session.visible_pids.clone();
        let mut audible = session.audible_pids.clone();
        expand_protected_descendants(processes, session_id, &mut foreground);
        expand_protected_descendants(processes, session_id, &mut visible);
        expand_protected_descendants(processes, session_id, &mut audible);
        let cohort_contains = |protected: &BTreeSet<u32>| {
            processes.iter().any(|candidate| {
                candidate.session_id == Some(session_id)
                    && candidate
                        .image_name
                        .eq_ignore_ascii_case(&process.image_name)
                    && protected.contains(&candidate.key.pid)
            })
        };
        if config.background_efficiency.protect_foreground && cohort_contains(&foreground) {
            Some(BackgroundProtection::Foreground)
        } else if config.background_efficiency.protect_visible && cohort_contains(&visible) {
            Some(BackgroundProtection::Visible)
        } else if config.background_efficiency.protect_audio && cohort_contains(&audible) {
            Some(BackgroundProtection::Audio)
        } else {
            None
        }
    }

    fn desired_background_state(
        config: &ControllerConfig,
        original: ProcessEfficiencyState,
        low_memory: bool,
    ) -> ProcessEfficiencyState {
        ProcessEfficiencyState {
            eco_qos: if config.background_efficiency.eco_qos_enabled {
                ProcessEcoQosState::Enabled
            } else {
                original.eco_qos
            },
            memory_priority: if config.background_efficiency.memory_priority_enabled {
                let requested =
                    if low_memory && config.background_efficiency.memory_pressure_guard_enabled {
                        ProcessMemoryPriority::Low
                    } else {
                        ProcessMemoryPriority::BelowNormal
                    };
                lower_memory_priority(original.memory_priority, requested)
            } else {
                original.memory_priority
            },
        }
    }

    const fn lower_memory_priority(
        original: ProcessMemoryPriority,
        requested: ProcessMemoryPriority,
    ) -> ProcessMemoryPriority {
        if memory_priority_rank(original) <= memory_priority_rank(requested) {
            original
        } else {
            requested
        }
    }

    const fn memory_priority_rank(priority: ProcessMemoryPriority) -> u8 {
        match priority {
            ProcessMemoryPriority::VeryLow => 1,
            ProcessMemoryPriority::Low => 2,
            ProcessMemoryPriority::Medium => 3,
            ProcessMemoryPriority::BelowNormal => 4,
            ProcessMemoryPriority::Normal => 5,
        }
    }

    const fn configured_background_ownership(
        config: &ControllerConfig,
    ) -> ProcessEfficiencyOwnership {
        ProcessEfficiencyOwnership {
            eco_qos: config.background_efficiency.eco_qos_enabled,
            memory_priority: config.background_efficiency.memory_priority_enabled,
        }
    }

    fn external_override_mask(
        expected: ProcessEfficiencyState,
        observed: ProcessEfficiencyState,
        ownership: ProcessEfficiencyOwnership,
    ) -> ProcessEfficiencyOwnership {
        ProcessEfficiencyOwnership {
            eco_qos: ownership.eco_qos && expected.eco_qos != observed.eco_qos,
            memory_priority: ownership.memory_priority
                && expected.memory_priority != observed.memory_priority,
        }
    }

    fn rebase_unowned_efficiency(
        record: &mut ManagedBackgroundProcess,
        observed: ProcessEfficiencyState,
    ) {
        if !record.ownership.eco_qos {
            record.original.eco_qos = observed.eco_qos;
            record.applied.eco_qos = observed.eco_qos;
        }
        if !record.ownership.memory_priority {
            record.original.memory_priority = observed.memory_priority;
            record.applied.memory_priority = observed.memory_priority;
        }
    }

    fn relinquish_external_overrides(
        record: &mut ManagedBackgroundProcess,
        observed: ProcessEfficiencyState,
        changed: ProcessEfficiencyOwnership,
    ) {
        record.ownership = record.ownership.without(changed);
        record.blocked_by_external_override = record.blocked_by_external_override.union(changed);
        if changed.eco_qos {
            record.original.eco_qos = observed.eco_qos;
            record.applied.eco_qos = observed.eco_qos;
        }
        if changed.memory_priority {
            record.original.memory_priority = observed.memory_priority;
            record.applied.memory_priority = observed.memory_priority;
        }
    }

    fn apply_restore_report_to_record(
        record: &mut ManagedBackgroundProcess,
        report: &platform::EfficiencyMutationReport,
    ) -> ProcessEfficiencyOwnership {
        let externally_overridden = ProcessEfficiencyOwnership {
            eco_qos: report.external_eco_qos_preserved,
            memory_priority: report.external_memory_priority_preserved,
        };
        record.applied = report.observed;
        if !report.unrestored_ownership.eco_qos {
            record.original.eco_qos = report.observed.eco_qos;
        }
        if !report.unrestored_ownership.memory_priority {
            record.original.memory_priority = report.observed.memory_priority;
        }
        record.ownership = report.unrestored_ownership;
        record.pending = None;
        record.pending_ownership = None;
        record.blocked_by_external_override = record
            .blocked_by_external_override
            .union(externally_overridden);
        externally_overridden
    }

    #[allow(clippy::too_many_arguments)]
    fn run_background_safety_tick(
        config: &ControllerConfig,
        runtime: &RuntimeState,
        topology: &Topology,
        interactive_server: Option<&platform::InteractiveStateServer>,
        memory_pressure_monitor: Option<&platform::MemoryPressureMonitor>,
        last_low_memory: &mut Option<bool>,
        previous_cpu_times: &mut BTreeMap<ProcessKey, u64>,
        clear_streaks: &mut BTreeMap<ProcessKey, u8>,
        managed: &mut ManagedAssignments,
        managed_state_path: Option<&Path>,
        managed_background: &mut ManagedBackground,
        background_state_path: Option<&Path>,
        status: &mut ControllerStatus,
        logger: &mut EventLogger,
    ) -> Result<BackgroundSafetyTick, ServiceError> {
        let processes = platform::observe_processes(topology)?;
        let live = processes
            .iter()
            .map(|process| process.key)
            .collect::<BTreeSet<_>>();
        reconcile_managed(logger, managed, managed_state_path, &processes)?;
        previous_cpu_times.retain(|key, _| live.contains(key));
        clear_streaks.retain(|key, _| live.contains(key));

        let interactive_states =
            interactive_server.map_or_else(Vec::new, platform::InteractiveStateServer::states);
        let (low_memory, memory_pressure_monitor_available) = match memory_pressure_monitor {
            Some(monitor) => match monitor.is_low() {
                Ok(value) => {
                    *last_low_memory = Some(value);
                    (Some(value), true)
                }
                Err(_) => (*last_low_memory, false),
            },
            None => (None, false),
        };
        let prior_status = status.background_efficiency.clone();
        let report = reconcile_background_efficiency(
            config,
            controller_mutations_active(config, runtime),
            &processes,
            &interactive_states,
            low_memory,
            memory_pressure_monitor_available,
            clear_streaks,
            managed_background,
            background_state_path,
            logger,
        )?;
        let mut background_status = report.status;
        if background_status.last_action.is_none() {
            background_status
                .last_action
                .clone_from(&prior_status.last_action);
        }
        status.background_efficiency = background_status;
        if let Some(error) = &report.last_error {
            status.last_error = Some(format!("Background efficiency: {error}"));
        } else if status
            .last_error
            .as_deref()
            .is_some_and(|error| error.starts_with("Background efficiency:"))
        {
            status.last_error = None;
        }
        Ok(BackgroundSafetyTick {
            processes,
            prior_status,
            error: report.last_error,
        })
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn reconcile_background_efficiency(
        config: &ControllerConfig,
        mutations_active: bool,
        processes: &[platform::ObservedProcess],
        interactive_states: &[InteractiveActivityState],
        low_memory: Option<bool>,
        memory_pressure_monitor_available: bool,
        clear_streaks: &mut BTreeMap<ProcessKey, u8>,
        managed: &mut ManagedBackground,
        state_path: Option<&Path>,
        logger: &mut EventLogger,
    ) -> Result<BackgroundReconcileReport, ServiceError> {
        let now = unix_time_ms();
        let sessions = interactive_sessions(interactive_states, now);
        let low_memory_condition = low_memory.unwrap_or(false);
        let mut status = BackgroundEfficiencyStatus {
            memory_pressure_monitor_available,
            low_memory_condition,
            ..BackgroundEfficiencyStatus::default()
        };
        let mut last_error = None;
        let live = processes
            .iter()
            .map(|process| process.key)
            .collect::<BTreeSet<_>>();
        let mut required_sessions = BTreeSet::<u32>::new();
        let dead = managed
            .keys()
            .filter(|key| {
                !live.contains(key)
                    && !processes.iter().any(|process| {
                        process.key.pid == key.pid && process.key.creation_time_100ns == 0
                    })
            })
            .copied()
            .collect::<Vec<_>>();
        if !dead.is_empty() {
            for key in dead {
                managed.remove(&key);
            }
            persist_background_state(state_path, managed)?;
        }

        let mut mutation_budget = usize::from(config.policy.max_mutations_per_evaluation);
        for process in processes {
            let exact_background = config.background_efficiency_applies(&process.image_name)
                && process.exclusion.is_none()
                && process.session_id.is_some_and(|session| session != 0);
            if exact_background {
                status.eligible_processes = status.eligible_processes.saturating_add(1);
                if let Some(session_id) = process.session_id {
                    required_sessions.insert(session_id);
                }
            }
            let protection = exact_background
                .then(|| background_protection(config, process, processes, &sessions))
                .flatten();
            let externally_blocked = managed
                .get(&process.key)
                .is_some_and(|record| !record.blocked_by_external_override.is_empty());
            let desired_allowed = exact_background && protection.is_none() && mutations_active;
            if exact_background && (protection.is_some() || !mutations_active || externally_blocked)
            {
                status.protected_processes = status.protected_processes.saturating_add(1);
            }

            if desired_allowed {
                let streak = clear_streaks.entry(process.key).or_default();
                *streak = streak.saturating_add(1);
            } else {
                clear_streaks.remove(&process.key);
            }

            if let Some(mut record) = managed.get(&process.key).copied() {
                let mut current = match platform::query_process_efficiency_key(process.key) {
                    Ok(current) => current,
                    Err(error) => {
                        if error.process_no_longer_matches() {
                            managed.remove(&process.key);
                            persist_background_state(state_path, managed)?;
                            continue;
                        }
                        last_error = Some(format!(
                            "background state query failed for {} (PID {}): {error}",
                            process.image_name, process.key.pid
                        ));
                        continue;
                    }
                };
                if let Some(pending) = record.pending {
                    if record.ownership.is_empty() {
                        record.pending = None;
                        record.pending_ownership = None;
                        rebase_unowned_efficiency(&mut record, current);
                        managed.insert(process.key, record);
                        persist_background_state(state_path, managed)?;
                        continue;
                    }
                    let final_ownership = record.pending_ownership.unwrap_or_else(|| {
                        ProcessEfficiencyOwnership::between(record.original, pending)
                    });
                    let transaction_ownership = record.ownership;
                    if transaction_ownership.matches(pending, current) {
                        record.applied = current;
                        record.ownership = final_ownership;
                        record.pending = None;
                        record.pending_ownership = None;
                        rebase_unowned_efficiency(&mut record, current);
                        managed.insert(process.key, record);
                        persist_background_state(state_path, managed)?;
                    } else if transaction_ownership.matches(record.applied, current) {
                        record.ownership =
                            ProcessEfficiencyOwnership::between(record.original, record.applied)
                                .without(record.blocked_by_external_override);
                        record.pending = None;
                        record.pending_ownership = None;
                        rebase_unowned_efficiency(&mut record, current);
                        managed.insert(process.key, record);
                        persist_background_state(state_path, managed)?;
                    } else {
                        match platform::restore_process_efficiency_key(
                            process.key,
                            record.original,
                            record.applied,
                            transaction_ownership,
                            record.pending,
                        ) {
                            Ok(report) => {
                                let changed = apply_restore_report_to_record(&mut record, &report);
                                if !report.property_errors.is_empty() {
                                    last_error = Some(report.property_errors.join("; "));
                                }
                                managed.insert(process.key, record);
                                persist_background_state(state_path, managed)?;
                                logger.emit(json!({
                                    "event": "background_efficiency_pending_recovered",
                                    "process": process.key,
                                    "externally_overridden": changed,
                                    "report": report,
                                }));
                            }
                            Err(error) => {
                                last_error = Some(error.to_string());
                            }
                        }
                        continue;
                    }
                    current = match platform::query_process_efficiency_key(process.key) {
                        Ok(current) => current,
                        Err(error) => {
                            last_error = Some(error.to_string());
                            continue;
                        }
                    };
                }

                record = *managed
                    .get(&process.key)
                    .expect("managed background record remains present");
                let changed = external_override_mask(record.applied, current, record.ownership);
                if !changed.is_empty() {
                    relinquish_external_overrides(&mut record, current, changed);
                    clear_streaks.remove(&process.key);
                    managed.insert(process.key, record);
                    persist_background_state(state_path, managed)?;
                    logger.emit(json!({
                        "event": "background_efficiency_external_override",
                        "process": process.key,
                        "relinquished": changed,
                        "observed": current,
                    }));
                }

                if !desired_allowed {
                    let transient_protection =
                        exact_background && mutations_active && protection.is_some();
                    if record.ownership.is_empty() {
                        if !transient_protection || record.blocked_by_external_override.is_empty() {
                            managed.remove(&process.key);
                        }
                        persist_background_state(state_path, managed)?;
                        continue;
                    }
                    match platform::restore_process_efficiency_key(
                        process.key,
                        record.original,
                        record.applied,
                        record.ownership,
                        None,
                    ) {
                        Ok(report) => {
                            let externally_overridden =
                                apply_restore_report_to_record(&mut record, &report);
                            let cleanup_incomplete = !record.ownership.is_empty();
                            if cleanup_incomplete
                                || (transient_protection
                                    && !record.blocked_by_external_override.is_empty())
                            {
                                managed.insert(process.key, record);
                            } else {
                                managed.remove(&process.key);
                            }
                            persist_background_state(state_path, managed)?;
                            let action = if cleanup_incomplete {
                                format!(
                                    "Background policy cleanup remains pending for {} (PID {})",
                                    process.image_name, process.key.pid
                                )
                            } else {
                                format!(
                                    "Restored background policy for {} (PID {})",
                                    process.image_name, process.key.pid
                                )
                            };
                            status.last_action = Some(action);
                            if !report.property_errors.is_empty() {
                                last_error = Some(report.property_errors.join("; "));
                            }
                            logger.emit(json!({
                                "event": "background_efficiency_restored",
                                "process": process.key,
                                "protection": protection.map(|value| format!("{value:?}")),
                                "externally_overridden": externally_overridden,
                                "report": report,
                            }));
                        }
                        Err(error) => last_error = Some(error.to_string()),
                    }
                    continue;
                }

                rebase_unowned_efficiency(&mut record, current);
                let desired =
                    desired_background_state(config, record.original, low_memory_condition);
                let desired_ownership =
                    ProcessEfficiencyOwnership::between(record.original, desired)
                        .intersection(configured_background_ownership(config))
                        .without(record.blocked_by_external_override);
                let prior_ownership = record.ownership;
                let transaction_ownership = prior_ownership.union(desired_ownership);
                if transaction_ownership.is_empty() {
                    managed.insert(process.key, record);
                    continue;
                }
                if transaction_ownership.matches(desired, record.applied)
                    && prior_ownership == desired_ownership
                {
                    continue;
                }
                if prior_ownership.is_empty()
                    && clear_streaks.get(&process.key).copied().unwrap_or(0)
                        < BACKGROUND_CLEAR_STABILITY_SAMPLES
                {
                    continue;
                }
                if mutation_budget == 0 {
                    continue;
                }
                record.pending = Some(desired);
                record.pending_ownership = Some(desired_ownership);
                record.ownership = transaction_ownership;
                managed.insert(process.key, record);
                persist_background_state(state_path, managed)?;
                mutation_budget = mutation_budget.saturating_sub(1);
                match platform::apply_process_efficiency_key(
                    process.key,
                    record.applied,
                    desired,
                    transaction_ownership,
                ) {
                    Ok(report) => {
                        record.applied = report.observed;
                        record.ownership = desired_ownership;
                        record.pending = None;
                        record.pending_ownership = None;
                        rebase_unowned_efficiency(&mut record, report.observed);
                        managed.insert(process.key, record);
                        persist_background_state(state_path, managed)?;
                        status.last_action = Some(format!(
                            "Updated background policy for {} (PID {})",
                            process.image_name, process.key.pid
                        ));
                        logger.emit(json!({
                            "event": "background_efficiency_updated",
                            "process": process.key,
                            "low_memory": low_memory_condition,
                            "report": report,
                        }));
                    }
                    Err(error) => {
                        last_error = Some(error.to_string());
                        if error.efficiency_ownership_changed()
                            && let Ok(current) = platform::query_process_efficiency_key(process.key)
                        {
                            let changed = external_override_mask(
                                record.applied,
                                current,
                                transaction_ownership,
                            );
                            record.ownership = prior_ownership;
                            record.pending = None;
                            record.pending_ownership = None;
                            relinquish_external_overrides(&mut record, current, changed);
                            rebase_unowned_efficiency(&mut record, current);
                            clear_streaks.remove(&process.key);
                            managed.insert(process.key, record);
                            persist_background_state(state_path, managed)?;
                        }
                    }
                }
                continue;
            }

            if !desired_allowed
                || clear_streaks.get(&process.key).copied().unwrap_or(0)
                    < BACKGROUND_CLEAR_STABILITY_SAMPLES
                || mutation_budget == 0
            {
                continue;
            }
            let original = match platform::query_process_efficiency_key(process.key) {
                Ok(original) => original,
                Err(error) => {
                    last_error = Some(format!(
                        "background efficiency unsupported for {} (PID {}): {error}",
                        process.image_name, process.key.pid
                    ));
                    continue;
                }
            };
            let desired = desired_background_state(config, original, low_memory_condition);
            let desired_ownership = ProcessEfficiencyOwnership::between(original, desired)
                .intersection(configured_background_ownership(config));
            if desired_ownership.is_empty() {
                continue;
            }
            let mut record = ManagedBackgroundProcess {
                key: process.key,
                original,
                applied: original,
                ownership: desired_ownership,
                pending: Some(desired),
                pending_ownership: Some(desired_ownership),
                blocked_by_external_override: ProcessEfficiencyOwnership::default(),
            };
            managed.insert(process.key, record);
            persist_background_state(state_path, managed)?;
            mutation_budget = mutation_budget.saturating_sub(1);
            match platform::apply_process_efficiency_key(
                process.key,
                original,
                desired,
                desired_ownership,
            ) {
                Ok(report) => {
                    record.applied = report.observed;
                    record.pending = None;
                    record.pending_ownership = None;
                    rebase_unowned_efficiency(&mut record, report.observed);
                    managed.insert(process.key, record);
                    persist_background_state(state_path, managed)?;
                    status.last_action = Some(format!(
                        "Applied background policy to {} (PID {})",
                        process.image_name, process.key.pid
                    ));
                    logger.emit(json!({
                        "event": "background_efficiency_applied",
                        "process": process.key,
                        "low_memory": low_memory_condition,
                        "report": report,
                    }));
                }
                Err(error) => {
                    last_error = Some(error.to_string());
                    if let Ok(current) = platform::query_process_efficiency_key(process.key) {
                        if error.efficiency_ownership_changed() {
                            let changed =
                                external_override_mask(original, current, desired_ownership);
                            record.original = current;
                            record.applied = current;
                            record.ownership = ProcessEfficiencyOwnership::default();
                            record.pending = None;
                            record.pending_ownership = None;
                            record.blocked_by_external_override = changed;
                            clear_streaks.remove(&process.key);
                            managed.insert(process.key, record);
                            persist_background_state(state_path, managed)?;
                        } else if desired_ownership.matches(original, current) {
                            managed.remove(&process.key);
                            persist_background_state(state_path, managed)?;
                        }
                    }
                }
            }
        }
        status.required_probe_sessions = required_sessions.len();
        status.interactive_probe_sessions = required_sessions
            .iter()
            .filter(|session_id| {
                sessions.get(session_id).is_some_and(|session| {
                    (!(config.background_efficiency.protect_foreground
                        || config.background_efficiency.protect_visible)
                        || session.window_probe_available)
                        && (!config.background_efficiency.protect_audio
                            || session.audio_probe_available)
                })
            })
            .count();
        status.managed_processes = managed
            .values()
            .filter(|record| !record.ownership.is_empty())
            .count();
        Ok(BackgroundReconcileReport { status, last_error })
    }

    fn cleanup_background(
        logger: &mut EventLogger,
        managed: &mut ManagedBackground,
        state_path: Option<&Path>,
    ) -> Result<CleanupReport, ServiceError> {
        let entries = managed.values().copied().collect::<Vec<_>>();
        let mut report = CleanupReport {
            attempted: entries.len(),
            ..CleanupReport::default()
        };
        for mut record in entries {
            if record.ownership.is_empty() {
                managed.remove(&record.key);
                report.cleared = report.cleared.saturating_add(1);
                continue;
            }
            let result = platform::restore_process_efficiency_key(
                record.key,
                record.original,
                record.applied,
                record.ownership,
                record.pending,
            );
            let target_gone = result
                .as_ref()
                .is_err_and(winsched::platform::PlatformError::process_no_longer_matches);
            let mut mutation_report = None;
            let mut error = None;
            let succeeded = match result {
                Ok(restored) => {
                    apply_restore_report_to_record(&mut record, &restored);
                    let complete = record.ownership.is_empty();
                    if complete {
                        managed.remove(&record.key);
                        report.cleared = report.cleared.saturating_add(1);
                    } else {
                        managed.insert(record.key, record);
                        report.failed = report.failed.saturating_add(1);
                        error = Some(restored.property_errors.join("; "));
                    }
                    mutation_report = Some(restored);
                    complete
                }
                Err(_) if target_gone => {
                    managed.remove(&record.key);
                    report.cleared = report.cleared.saturating_add(1);
                    true
                }
                Err(restore_error) => {
                    report.failed = report.failed.saturating_add(1);
                    error = Some(restore_error.to_string());
                    false
                }
            };
            logger.emit(json!({
                "event": "background_efficiency_cleanup",
                "process": record.key,
                "succeeded": succeeded,
                "target_gone_or_reused": target_gone,
                "report": mutation_report.as_ref(),
                "error": error,
            }));
        }
        persist_background_state(state_path, managed)?;
        Ok(report)
    }

    fn read_state_with_legacy_backup(path: &Path) -> Result<Option<Vec<u8>>, std::io::Error> {
        if path.exists() {
            return fs::read(path).map(Some);
        }
        let backup = path.with_extension("bak");
        if !backup.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&backup)?;
        // Older builds briefly moved the live file to .bak before committing
        // the replacement. Restore that durable journal after an interrupted write.
        match fs::rename(&backup, path) {
            Ok(()) => {}
            Err(_) if path.exists() => {}
            Err(error) => return Err(error),
        }
        Ok(Some(bytes))
    }

    fn load_background_state(path: &Path) -> Result<ManagedBackground, ServiceError> {
        let Some(bytes) = read_state_with_legacy_backup(path)? else {
            return Ok(BTreeMap::new());
        };
        let header = serde_json::from_slice::<ManagedStateHeader>(&bytes)?;
        if header.schema_version == 1 {
            let state = serde_json::from_slice::<LegacyBackgroundStateFile>(&bytes)?;
            return Ok(state
                .processes
                .into_iter()
                .map(|process| {
                    let pending = if process.blocked_by_external_override {
                        None
                    } else {
                        process.pending
                    };
                    let pending_ownership = pending.map(|pending| {
                        ProcessEfficiencyOwnership::between(process.original, pending)
                    });
                    let ownership = if process.blocked_by_external_override {
                        ProcessEfficiencyOwnership::default()
                    } else {
                        ProcessEfficiencyOwnership::between(process.original, process.applied)
                            .union(pending_ownership.unwrap_or_default())
                    };
                    let blocked_by_external_override = if process.blocked_by_external_override {
                        ProcessEfficiencyOwnership {
                            eco_qos: true,
                            memory_priority: true,
                        }
                    } else {
                        ProcessEfficiencyOwnership::default()
                    };
                    let migrated = ManagedBackgroundProcess {
                        key: process.key,
                        original: process.original,
                        applied: process.applied,
                        ownership,
                        pending,
                        pending_ownership,
                        blocked_by_external_override,
                    };
                    (migrated.key, migrated)
                })
                .collect());
        }
        if header.schema_version != BACKGROUND_STATE_SCHEMA_VERSION {
            return Err(ServiceError::StateSchema(header.schema_version));
        }
        let state = serde_json::from_slice::<BackgroundStateFile>(&bytes)?;
        for process in &state.processes {
            if process.pending.is_some() != process.pending_ownership.is_some() {
                return Err(ServiceError::InvalidBackgroundState(
                    "pending state and pending ownership must appear together",
                ));
            }
            if process.pending.is_some() && process.ownership.is_empty() {
                return Err(ServiceError::InvalidBackgroundState(
                    "pending transaction has an empty ownership mask",
                ));
            }
            if !process
                .ownership
                .intersection(process.blocked_by_external_override)
                .is_empty()
            {
                return Err(ServiceError::InvalidBackgroundState(
                    "owned and externally blocked properties overlap",
                ));
            }
            if process.pending_ownership.is_some_and(|pending| {
                !pending
                    .intersection(process.blocked_by_external_override)
                    .is_empty()
            }) {
                return Err(ServiceError::InvalidBackgroundState(
                    "pending ownership includes an externally blocked property",
                ));
            }
        }
        Ok(state
            .processes
            .into_iter()
            .map(|process| (process.key, process))
            .collect())
    }

    fn persist_background_state(
        path: Option<&Path>,
        managed: &ManagedBackground,
    ) -> Result<(), ServiceError> {
        let Some(path) = path else {
            return Ok(());
        };
        let state = BackgroundStateFile {
            schema_version: BACKGROUND_STATE_SCHEMA_VERSION,
            processes: managed.values().copied().collect(),
        };
        atomic_write(path, &serde_json::to_vec_pretty(&state)?)?;
        Ok(())
    }

    fn load_managed_state(path: &Path) -> Result<ManagedAssignments, ServiceError> {
        let Some(bytes) = read_state_with_legacy_backup(path)? else {
            return Ok(BTreeMap::new());
        };
        let header = serde_json::from_slice::<ManagedStateHeader>(&bytes)?;
        match header.schema_version {
            LEGACY_STATE_SCHEMA_VERSION => {
                let state = serde_json::from_slice::<LegacyManagedStateFile>(&bytes)?;
                Ok(state
                    .processes
                    .into_iter()
                    .map(|process| {
                        (
                            process.key,
                            ManagedPlacement {
                                anchor_domain: process.domain,
                                cpu_set_ids: Vec::new(),
                            },
                        )
                    })
                    .collect())
            }
            STATE_SCHEMA_VERSION => {
                let state = serde_json::from_slice::<ManagedStateFile>(&bytes)?;
                Ok(state
                    .processes
                    .into_iter()
                    .map(|process| (process.key, process.placement))
                    .collect())
            }
            schema => Err(ServiceError::StateSchema(schema)),
        }
    }

    fn persist_managed_state(
        path: Option<&Path>,
        managed: &ManagedAssignments,
    ) -> Result<(), ServiceError> {
        let Some(path) = path else {
            return Ok(());
        };
        let state = ManagedStateFile {
            schema_version: STATE_SCHEMA_VERSION,
            processes: managed
                .iter()
                .map(|(key, placement)| ManagedProcess {
                    key: *key,
                    placement: placement.clone(),
                })
                .collect(),
        };
        atomic_write(path, &serde_json::to_vec_pretty(&state)?)?;
        Ok(())
    }

    fn load_runtime_state(
        path: &Path,
        configured_mode: ControllerMode,
    ) -> Result<RuntimeState, ServiceError> {
        let Some(bytes) = read_state_with_legacy_backup(path)? else {
            return Ok(RuntimeState::for_controller_mode(configured_mode));
        };
        let state = serde_json::from_slice::<RuntimeState>(&bytes)?;
        if state.schema_version != RUNTIME_SCHEMA_VERSION {
            return Err(ServiceError::RuntimeStateSchema(state.schema_version));
        }
        Ok(state)
    }

    fn persist_runtime_state(
        path: Option<&Path>,
        state: &RuntimeState,
    ) -> Result<(), ServiceError> {
        let Some(path) = path else {
            return Ok(());
        };
        atomic_write(path, &serde_json::to_vec_pretty(state)?)?;
        Ok(())
    }

    fn persist_controller_status(
        path: Option<&Path>,
        status: &mut ControllerStatus,
    ) -> Result<(), ServiceError> {
        let Some(path) = path else {
            return Ok(());
        };
        status.updated_at_unix_ms = unix_time_ms();
        atomic_write(path, &serde_json::to_vec_pretty(status)?)?;
        Ok(())
    }

    fn publish_controller_status(
        gate: &mut StatusPublishGate,
        path: Option<&Path>,
        status: &mut ControllerStatus,
        monotonic_ms: u64,
        force: bool,
    ) -> Result<(), ServiceError> {
        if !gate.should_publish(monotonic_ms, force) {
            return Ok(());
        }
        persist_controller_status(path, status)?;
        gate.mark_published(monotonic_ms);
        Ok(())
    }

    fn controller_elapsed_ms(started: Instant) -> u64 {
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn install(
        config_path: &Path,
        data_directory: Option<&Path>,
        start_now: bool,
        allow_auto: bool,
    ) -> Result<(), ServiceError> {
        let config = validated_registration_config(config_path, allow_auto)?;
        let install_dir = data_directory.map_or_else(
            || program_data_dir().join(INSTALL_DIRECTORY_NAME),
            Path::to_path_buf,
        );
        fs::create_dir_all(&install_dir)?;
        let installed_exe = install_dir.join("winsched-service.exe");
        let installed_config = install_dir.join(CONFIG_FILE_NAME);
        let current_exe = std::env::current_exe()?;
        if current_exe != installed_exe {
            fs::copy(&current_exe, &installed_exe)?;
        }
        if !paths_refer_to_same_file(config_path, &installed_config)? {
            atomic_write(
                &installed_config,
                toml::to_string_pretty(&config)?.as_bytes(),
            )?;
        }
        configure_service(&installed_exe, &installed_config, start_now, true, false)?;
        println!("installed {SERVICE_NAME}");
        Ok(())
    }

    fn paths_refer_to_same_file(left: &Path, right: &Path) -> Result<bool, std::io::Error> {
        if !left.exists() || !right.exists() {
            return Ok(false);
        }
        Ok(fs::canonicalize(left)? == fs::canonicalize(right)?)
    }

    fn register_in_place(
        config_path: &Path,
        start_now: bool,
        allow_auto: bool,
    ) -> Result<(), ServiceError> {
        let config = validated_registration_config(config_path, allow_auto)?;
        let _ = config;
        let current_exe = std::env::current_exe()?;
        let absolute_config = fs::canonicalize(config_path)?;
        configure_service(&current_exe, &absolute_config, start_now, false, false)?;
        println!("registered {SERVICE_NAME}");
        Ok(())
    }

    fn provision_in_place(
        config_path: &Path,
        start_now: bool,
        allow_auto: bool,
        test_fail_after_change: bool,
    ) -> Result<(), ServiceError> {
        let config = validated_registration_config(config_path, allow_auto)?;
        let _ = config;
        let current_exe = std::env::current_exe()?;
        let absolute_config = fs::canonicalize(config_path)?;
        configure_service(
            &current_exe,
            &absolute_config,
            start_now,
            true,
            test_fail_after_change,
        )?;
        println!("provisioned {SERVICE_NAME}");
        Ok(())
    }

    fn validated_registration_config(
        config_path: &Path,
        allow_auto: bool,
    ) -> Result<ControllerConfig, ServiceError> {
        let config = load_config(config_path)?;
        if config.controller_mode == ControllerMode::Auto && !allow_auto {
            return Err(ServiceError::AutoNeedsConfirmation);
        }
        Ok(config)
    }

    fn configure_service(
        executable: &Path,
        config_path: &Path,
        start_now: bool,
        allow_existing: bool,
        test_fail_after_change: bool,
    ) -> Result<(), ServiceError> {
        let manager = ServiceManager::local_computer(
            None::<&str>,
            ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
        )?;
        let info = ServiceInfo {
            name: OsString::from(SERVICE_NAME),
            display_name: OsString::from(SERVICE_DISPLAY_NAME),
            service_type: SERVICE_TYPE,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: executable.to_owned(),
            launch_arguments: vec![
                OsString::from("service"),
                OsString::from("--config"),
                config_path.as_os_str().to_owned(),
            ],
            dependencies: Vec::new(),
            account_name: Some(OsString::from("LocalSystem")),
            account_password: None,
        };
        let access = ServiceAccess::CHANGE_CONFIG
            | ServiceAccess::DELETE
            | ServiceAccess::QUERY_CONFIG
            | ServiceAccess::QUERY_STATUS
            | ServiceAccess::START
            | ServiceAccess::STOP;
        let (service, origin) = if allow_existing {
            match manager.open_service(SERVICE_NAME, access) {
                Ok(service) => {
                    let snapshot = capture_existing_service(&service)?;
                    if let Err(error) = service.change_config(&info) {
                        let operation = ServiceError::from(error);
                        return Err(with_transaction_recovery(
                            operation,
                            rollback_existing_service(&service, &snapshot),
                        ));
                    }
                    (service, ServiceOrigin::Existing(Box::new(snapshot)))
                }
                Err(error) if is_missing_service_error(&error) => (
                    manager.create_service(&info, access)?,
                    ServiceOrigin::Created,
                ),
                Err(error) => return Err(error.into()),
            }
        } else {
            (
                manager.create_service(&info, access)?,
                ServiceOrigin::Created,
            )
        };

        let configure_result = if test_fail_after_change {
            Err(ServiceError::InjectedProvisionFailure)
        } else {
            apply_service_settings(&service, start_now)
        };
        if let Err(error) = configure_result {
            let recovery = match origin {
                ServiceOrigin::Created => cleanup_created_service(&manager, service),
                ServiceOrigin::Existing(snapshot) => rollback_existing_service(&service, &snapshot),
            };
            return Err(with_transaction_recovery(error, recovery));
        }
        Ok(())
    }

    fn apply_service_settings(service: &Service, start_now: bool) -> Result<(), ServiceError> {
        service.set_description(
            "Supported-API-only, fail-closed LLC-aware CPU Set controller for Windows 11",
        )?;
        service.update_failure_actions(ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(Duration::from_hours(24)),
            reboot_msg: None,
            command: None,
            actions: Some(vec![
                ServiceAction {
                    action_type: ServiceActionType::Restart,
                    delay: Duration::from_secs(5),
                },
                ServiceAction {
                    action_type: ServiceActionType::Restart,
                    delay: Duration::from_secs(15),
                },
                ServiceAction {
                    action_type: ServiceActionType::Restart,
                    delay: Duration::from_mins(1),
                },
            ]),
        })?;
        service.set_failure_actions_on_non_crash_failures(true)?;
        grant_interactive_service_control()?;
        if start_now {
            ensure_service_running(service)?;
        }
        Ok(())
    }

    fn capture_existing_service(
        service: &Service,
    ) -> Result<ExistingServiceSnapshot, ServiceError> {
        let config = service.query_config()?;
        service_config_restore_args(&config)
            .map_err(ServiceError::UnsupportedServiceConfiguration)?;
        let state = stable_prior_service_state(service)?;
        let description = query_service_description()?;
        let failure_actions = service.get_failure_actions()?;
        let failure_actions_on_non_crash = service.get_failure_actions_on_non_crash_failures()?;
        let sddl = query_service_sddl()?;
        Ok(ExistingServiceSnapshot {
            config,
            state,
            description,
            failure_actions,
            failure_actions_on_non_crash,
            sddl,
        })
    }

    fn stable_prior_service_state(service: &Service) -> Result<PriorServiceState, ServiceError> {
        match service.query_status()?.current_state {
            ServiceState::Running => Ok(PriorServiceState::Running),
            ServiceState::Stopped => Ok(PriorServiceState::Stopped),
            ServiceState::StartPending => {
                wait_until_running(service)?;
                Ok(PriorServiceState::Running)
            }
            ServiceState::StopPending => {
                wait_until_stopped(service)?;
                Ok(PriorServiceState::Stopped)
            }
            _ => Err(ServiceError::UnsupportedServiceConfiguration(
                "service must be Running or Stopped before provisioning",
            )),
        }
    }

    fn service_config_restore_args(config: &ServiceConfig) -> Result<Vec<OsString>, &'static str> {
        if config.service_type != SERVICE_TYPE {
            return Err("only an OWN_PROCESS WinSched service is supported");
        }
        if config.load_order_group.is_some() || config.tag_id != 0 {
            return Err("load-order groups are not supported for WinSched");
        }
        if matches!(
            config.start_type,
            ServiceStartType::BootStart | ServiceStartType::SystemStart
        ) {
            return Err("driver start types are not supported for WinSched");
        }
        if !is_local_system_account(config.account_name.as_deref()) {
            return Err("the existing WinSched service must run as LocalSystem");
        }

        let mut dependencies = OsString::new();
        for dependency in &config.dependencies {
            if !dependencies.is_empty() {
                dependencies.push("/");
            }
            dependencies.push(dependency.to_system_identifier());
        }

        let mut args = vec![
            OsString::from("config"),
            OsString::from(SERVICE_NAME),
            OsString::from("type="),
            OsString::from("own"),
        ];
        // ChangeServiceConfigW does not touch delayed-auto-start state. Avoid spelling
        // `start= auto` through sc.exe when AutoStart was already the prior mode, because sc.exe
        // would clear an independently configured delayed-auto-start flag.
        if config.start_type != ServiceStartType::AutoStart {
            args.extend([
                OsString::from("start="),
                OsString::from(sc_start_type(config.start_type)),
            ]);
        }
        args.extend([
            OsString::from("error="),
            OsString::from(sc_error_control(config.error_control)),
            OsString::from("binPath="),
            config.executable_path.as_os_str().to_owned(),
            OsString::from("depend="),
            dependencies,
            OsString::from("obj="),
            OsString::from("LocalSystem"),
            OsString::from("DisplayName="),
            config.display_name.clone(),
        ]);
        Ok(args)
    }

    fn is_local_system_account(account: Option<&OsStr>) -> bool {
        account.is_some_and(|account| {
            let account = account.to_string_lossy();
            account.eq_ignore_ascii_case("LocalSystem")
                || account.eq_ignore_ascii_case(r"NT AUTHORITY\SYSTEM")
        })
    }

    const fn sc_start_type(start_type: ServiceStartType) -> &'static str {
        match start_type {
            ServiceStartType::BootStart => "boot",
            ServiceStartType::SystemStart => "system",
            ServiceStartType::AutoStart => "auto",
            ServiceStartType::OnDemand => "demand",
            ServiceStartType::Disabled => "disabled",
        }
    }

    const fn sc_error_control(error_control: ServiceErrorControl) -> &'static str {
        match error_control {
            ServiceErrorControl::Ignore => "ignore",
            ServiceErrorControl::Normal => "normal",
            ServiceErrorControl::Severe => "severe",
            ServiceErrorControl::Critical => "critical",
        }
    }

    fn rollback_existing_service(
        service: &Service,
        snapshot: &ExistingServiceSnapshot,
    ) -> Result<(), String> {
        let mut failures = Vec::new();

        // Always stop before restoring ImagePath. If the prior service was running, this
        // guarantees that the final restart loads the prior binary rather than merely leaving a
        // replacement process alive under a restored SCM registration.
        record_recovery_result(
            &mut failures,
            "stop replacement service",
            ensure_service_stopped(service),
        );

        match service_config_restore_args(&snapshot.config) {
            Ok(args) => record_recovery_result(
                &mut failures,
                "restore service configuration",
                run_system_sc(&args, "sc.exe config (rollback)"),
            ),
            Err(error) => failures.push(format!("restore service configuration: {error}")),
        }
        record_recovery_result(
            &mut failures,
            "restore service description",
            set_service_description(&snapshot.description),
        );

        let mut failure_actions = snapshot.failure_actions.clone();
        if failure_actions.actions.is_none() {
            failure_actions.actions = Some(Vec::new());
        }
        record_recovery_result(
            &mut failures,
            "restore service failure actions",
            service
                .update_failure_actions(failure_actions)
                .map_err(ServiceError::from),
        );
        record_recovery_result(
            &mut failures,
            "restore non-crash failure flag",
            service
                .set_failure_actions_on_non_crash_failures(snapshot.failure_actions_on_non_crash)
                .map_err(ServiceError::from),
        );
        record_recovery_result(
            &mut failures,
            "restore service ACL",
            set_service_sddl(&snapshot.sddl),
        );

        if snapshot.state == PriorServiceState::Running
            && snapshot.config.start_type == ServiceStartType::Disabled
        {
            record_recovery_result(
                &mut failures,
                "temporarily enable prior disabled service",
                set_service_start_type("demand"),
            );
            record_recovery_result(
                &mut failures,
                "restore service state",
                ensure_service_running(service),
            );
            record_recovery_result(
                &mut failures,
                "restore disabled start type",
                set_service_start_type("disabled"),
            );
        } else {
            let runtime_result = match snapshot.state {
                PriorServiceState::Running => ensure_service_running(service),
                PriorServiceState::Stopped => ensure_service_stopped(service),
            };
            record_recovery_result(&mut failures, "restore service state", runtime_result);
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    fn cleanup_created_service(manager: &ServiceManager, service: Service) -> Result<(), String> {
        let mut failures = Vec::new();
        record_recovery_result(
            &mut failures,
            "stop newly created service",
            ensure_service_stopped(&service),
        );
        if let Err(error) = service.delete()
            && !is_service_marked_for_delete_error(&error)
        {
            failures.push(format!("delete newly created service: {error}"));
        }
        drop(service);
        record_recovery_result(
            &mut failures,
            "wait for newly created service deletion",
            wait_until_absent(manager),
        );
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    fn record_recovery_result(
        failures: &mut Vec<String>,
        action: &'static str,
        result: Result<(), ServiceError>,
    ) {
        if let Err(error) = result {
            failures.push(format!("{action}: {error}"));
        }
    }

    fn with_transaction_recovery(
        operation: ServiceError,
        recovery: Result<(), String>,
    ) -> ServiceError {
        match recovery {
            Ok(()) => operation,
            Err(recovery) => ServiceError::TransactionRecovery {
                operation: Box::new(operation),
                recovery,
            },
        }
    }

    fn query_service_description() -> Result<OsString, ServiceError> {
        let args = [OsString::from("qdescription"), OsString::from(SERVICE_NAME)];
        let output = run_system_sc_output(&args, "sc.exe qdescription")?;
        parse_sc_description(&output.stdout).ok_or(ServiceError::InvalidCommandOutput {
            program: "sc.exe qdescription",
            detail: "description line was not found or was not UTF-8",
        })
    }

    fn query_service_sddl() -> Result<OsString, ServiceError> {
        let args = [OsString::from("sdshow"), OsString::from(SERVICE_NAME)];
        let output = run_system_sc_output(&args, "sc.exe sdshow")?;
        parse_sc_sddl(&output.stdout).ok_or(ServiceError::InvalidCommandOutput {
            program: "sc.exe sdshow",
            detail: "SDDL was not found or was not UTF-8",
        })
    }

    fn set_service_description(description: &OsStr) -> Result<(), ServiceError> {
        let args = [
            OsString::from("description"),
            OsString::from(SERVICE_NAME),
            description.to_owned(),
        ];
        run_system_sc(&args, "sc.exe description (rollback)")
    }

    fn set_service_sddl(sddl: &OsStr) -> Result<(), ServiceError> {
        let args = [
            OsString::from("sdset"),
            OsString::from(SERVICE_NAME),
            sddl.to_owned(),
        ];
        run_system_sc(&args, "sc.exe sdset (rollback)")
    }

    fn set_service_start_type(start_type: &'static str) -> Result<(), ServiceError> {
        let args = [
            OsString::from("config"),
            OsString::from(SERVICE_NAME),
            OsString::from("start="),
            OsString::from(start_type),
        ];
        run_system_sc(&args, "sc.exe config start (rollback)")
    }

    fn parse_sc_description(output: &[u8]) -> Option<OsString> {
        let mut values = output
            .split(|byte| *byte == b'\n')
            .filter_map(|line| {
                let line = trim_ascii(line);
                if line.starts_with(b"[SC]") {
                    return None;
                }
                let separator = line.iter().position(|byte| *byte == b':')?;
                std::str::from_utf8(trim_ascii(&line[separator + 1..]))
                    .ok()
                    .map(OsString::from)
            })
            .collect::<Vec<_>>();
        if values.len() < 2 {
            return None;
        }
        values.pop()
    }

    fn parse_sc_sddl(output: &[u8]) -> Option<OsString> {
        output.split(|byte| *byte == b'\n').rev().find_map(|line| {
            let candidate = trim_ascii(line);
            if candidate.windows(2).any(|window| window == b"D:")
                && !candidate.iter().any(u8::is_ascii_whitespace)
            {
                std::str::from_utf8(candidate).ok().map(OsString::from)
            } else {
                None
            }
        })
    }

    fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
        while bytes.first().is_some_and(u8::is_ascii_whitespace) {
            bytes = &bytes[1..];
        }
        while bytes.last().is_some_and(u8::is_ascii_whitespace) {
            bytes = &bytes[..bytes.len() - 1];
        }
        bytes
    }

    fn run_system_sc(args: &[OsString], program: &'static str) -> Result<(), ServiceError> {
        run_system_sc_output(args, program).map(|_| ())
    }

    fn run_system_sc_output(
        args: &[OsString],
        program: &'static str,
    ) -> Result<std::process::Output, ServiceError> {
        let output = ProcessCommand::new(system_sc_path()?).args(args).output()?;
        if output.status.success() {
            return Ok(output);
        }
        let stderr = command_failure_text(&output);
        Err(ServiceError::CommandFailed {
            program,
            exit_code: output.status.code(),
            stderr,
        })
    }

    fn command_failure_text(output: &std::process::Output) -> String {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if !stderr.is_empty() {
            return stderr;
        }
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if stdout.is_empty() {
            "no diagnostic output".to_owned()
        } else {
            stdout
        }
    }

    #[allow(unsafe_code)] // Read-only Win32 path query avoids inherited environment lookup.
    fn system_sc_path() -> Result<PathBuf, ServiceError> {
        // SAFETY: A null buffer asks Windows for the required UTF-16 length.
        let required = unsafe { GetSystemDirectoryW(None) };
        if required == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut buffer = vec![0u16; usize::try_from(required).unwrap_or(usize::MAX) + 1];
        // SAFETY: The buffer is writable and sized from the preceding Windows query.
        let written = unsafe { GetSystemDirectoryW(Some(buffer.as_mut_slice())) };
        if written == 0 || usize::try_from(written).unwrap_or(usize::MAX) >= buffer.len() {
            return Err(std::io::Error::last_os_error().into());
        }
        buffer.truncate(usize::try_from(written).expect("u32 fits usize"));
        Ok(PathBuf::from(OsString::from_wide(&buffer)).join("sc.exe"))
    }

    fn is_missing_service_error(error: &WindowsServiceError) -> bool {
        matches!(
            error,
            WindowsServiceError::Winapi(source) if source.raw_os_error() == Some(1060)
        )
    }

    fn is_service_marked_for_delete_error(error: &WindowsServiceError) -> bool {
        matches!(
            error,
            WindowsServiceError::Winapi(source) if source.raw_os_error() == Some(1072)
        )
    }

    fn grant_interactive_service_control() -> Result<(), ServiceError> {
        run_system_sc(
            &[
                OsString::from("sdset"),
                OsString::from(SERVICE_NAME),
                OsString::from(INTERACTIVE_SERVICE_SDDL),
            ],
            "sc.exe sdset",
        )
    }

    fn ensure_service_running(service: &Service) -> Result<(), ServiceError> {
        match service.query_status()?.current_state {
            ServiceState::Running => {}
            ServiceState::StartPending => wait_until_running(service)?,
            ServiceState::StopPending => {
                wait_until_stopped(service)?;
                service.start::<&OsStr>(&[])?;
                wait_until_running(service)?;
            }
            _ => {
                service.start::<&OsStr>(&[])?;
                wait_until_running(service)?;
            }
        }
        Ok(())
    }

    fn ensure_service_stopped(service: &Service) -> Result<(), ServiceError> {
        match service.query_status()?.current_state {
            ServiceState::Stopped => {}
            ServiceState::StopPending => wait_until_stopped(service)?,
            ServiceState::StartPending => {
                wait_until_running(service)?;
                service.stop()?;
                wait_until_stopped(service)?;
            }
            _ => {
                service.stop()?;
                wait_until_stopped(service)?;
            }
        }
        Ok(())
    }

    fn start() -> Result<(), ServiceError> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        let Some(service) = open_service_optional(
            &manager,
            SERVICE_NAME,
            ServiceAccess::START | ServiceAccess::QUERY_STATUS,
        )?
        else {
            println!("service is not installed");
            return Ok(());
        };
        ensure_service_running(&service)?;
        println!("service running");
        Ok(())
    }

    fn stop() -> Result<(), ServiceError> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        let Some(service) = open_service_optional(
            &manager,
            SERVICE_NAME,
            ServiceAccess::STOP | ServiceAccess::QUERY_STATUS,
        )?
        else {
            println!("service is not installed");
            return Ok(());
        };
        ensure_service_stopped(&service)?;
        let stopped = service.query_status()?;
        if stopped.exit_code != ServiceExitCode::Win32(0) {
            return Err(ServiceError::ServiceStoppedWithError(format!(
                "{:?}",
                stopped.exit_code
            )));
        }
        println!("service stopped");
        Ok(())
    }

    fn set_scheduling(enabled: bool) -> Result<(), ServiceError> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        let service = manager.open_service(
            SERVICE_NAME,
            ServiceAccess::USER_DEFINED_CONTROL | ServiceAccess::QUERY_STATUS,
        )?;
        let code = UserEventCode::from_raw(if enabled {
            CONTROL_ENABLE
        } else {
            CONTROL_DISABLE
        })
        .expect("WinSched control codes are in the documented 128..=255 range");
        let status = service.notify(code)?;
        println!(
            "scheduling {} requested: {:?}",
            if enabled { "enable" } else { "disable" },
            status.current_state
        );
        Ok(())
    }

    fn status() -> Result<(), ServiceError> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        let service = manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS)?;
        let status = service.query_status()?;
        println!("{:?}", status.current_state);
        Ok(())
    }

    fn cleanup_persisted_state(data_dir: &Path) -> Result<(), ServiceError> {
        let managed_path = data_dir.join(MANAGED_STATE_FILE_NAME);
        let background_path = data_dir.join(BACKGROUND_STATE_FILE_NAME);
        let mut logger = EventLogger::console();
        let managed_result = load_managed_state(&managed_path);
        let background_result = load_background_state(&background_path);
        let (mut managed, mut background) = match (managed_result, background_result) {
            (Ok(managed), Ok(background)) => (managed, background),
            (Ok(mut managed), Err(error)) => {
                let cleanup = cleanup_managed(
                    &mut logger,
                    &mut managed,
                    managed_path.exists().then_some(managed_path.as_path()),
                );
                return match cleanup {
                    Ok(report) if report.failed == 0 => Err(error),
                    cleanup => Err(ServiceError::TransactionRecovery {
                        operation: Box::new(error),
                        recovery: format!(
                            "placement cleanup after background journal load failure: {cleanup:?}"
                        ),
                    }),
                };
            }
            (Err(error), Ok(mut background)) => {
                let cleanup = cleanup_background(
                    &mut logger,
                    &mut background,
                    background_path
                        .exists()
                        .then_some(background_path.as_path()),
                );
                return match cleanup {
                    Ok(report) if report.failed == 0 => Err(error),
                    cleanup => Err(ServiceError::TransactionRecovery {
                        operation: Box::new(error),
                        recovery: format!(
                            "background cleanup after placement journal load failure: {cleanup:?}"
                        ),
                    }),
                };
            }
            (Err(error), Err(recovery)) => {
                return Err(ServiceError::TransactionRecovery {
                    operation: Box::new(error),
                    recovery: format!("background journal also failed to load: {recovery}"),
                });
            }
        };
        let placement = cleanup_managed(
            &mut logger,
            &mut managed,
            managed_path.exists().then_some(managed_path.as_path()),
        )?;
        let efficiency = cleanup_background(
            &mut logger,
            &mut background,
            background_path
                .exists()
                .then_some(background_path.as_path()),
        )?;
        if placement.failed != 0 || efficiency.failed != 0 {
            Err(ServiceError::CleanupIncomplete(
                placement.failed.saturating_add(efficiency.failed),
            ))
        } else {
            Ok(())
        }
    }

    fn uninstall(data_directory: Option<&Path>) -> Result<(), ServiceError> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        let data_dir = data_directory.map_or_else(
            || program_data_dir().join(INSTALL_DIRECTORY_NAME),
            Path::to_path_buf,
        );
        let Some(service) = open_service_optional(
            &manager,
            SERVICE_NAME,
            ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
        )?
        else {
            cleanup_persisted_state(&data_dir)?;
            println!("service is not installed");
            return Ok(());
        };
        ensure_service_stopped(&service)?;
        cleanup_persisted_state(&data_dir)?;
        if let Err(error) = service.delete()
            && !is_service_marked_for_delete_error(&error)
        {
            return Err(error.into());
        }
        drop(service);
        wait_until_absent(&manager)?;
        println!("service removed; ProgramData files were preserved");
        Ok(())
    }

    fn open_service_optional(
        manager: &ServiceManager,
        name: &str,
        access: ServiceAccess,
    ) -> Result<Option<Service>, ServiceError> {
        match manager.open_service(name, access) {
            Ok(service) => Ok(Some(service)),
            Err(error) if is_missing_service_error(&error) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn wait_until_stopped(service: &Service) -> Result<(), ServiceError> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if service.query_status()?.current_state == ServiceState::Stopped {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(ServiceError::ServiceStopTimeout);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_until_running(service: &Service) -> Result<(), ServiceError> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if service.query_status()?.current_state == ServiceState::Running {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(ServiceError::ServiceStartTimeout);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_until_absent(manager: &ServiceManager) -> Result<(), ServiceError> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
                Err(error) if is_missing_service_error(&error) => return Ok(()),
                Err(error) if is_service_marked_for_delete_error(&error) => {}
                Ok(service) => drop(service),
                Err(error) => return Err(error.into()),
            }
            if Instant::now() >= deadline {
                return Err(ServiceError::ServiceDeleteTimeout);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn program_data_dir() -> PathBuf {
        std::env::var_os("PROGRAMDATA")
            .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from)
    }

    fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
        platform::atomic_replace_file(path, bytes)
    }

    fn emergency_log(message: &str) -> Result<(), std::io::Error> {
        let directory = program_data_dir().join(INSTALL_DIRECTORY_NAME);
        fs::create_dir_all(&directory)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join("winsched-emergency.log"))?;
        writeln!(file, "{} {message}", unix_time_ms())
    }

    fn unix_time_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| {
                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
            })
    }

    struct EventLogger {
        sink: EventSink,
        last_write_error: Option<String>,
    }

    impl EventLogger {
        const fn console() -> Self {
            Self {
                sink: EventSink::console(),
                last_write_error: None,
            }
        }

        fn service(path: PathBuf, config: LoggingConfig) -> Result<Self, std::io::Error> {
            Ok(Self {
                sink: EventSink::service(path, config)?,
                last_write_error: None,
            })
        }

        fn reconfigure(&mut self, config: LoggingConfig) -> Result<(), std::io::Error> {
            self.sink.reconfigure(config)?;
            self.last_write_error = None;
            Ok(())
        }

        fn emit(&mut self, mut value: Value) {
            if let Some(object) = value.as_object_mut() {
                object.insert("timestamp_ms".to_owned(), json!(unix_time_ms()));
            }
            let line = serde_json::to_string(&value).expect("JSON Value serialization cannot fail");
            match self.sink.write_line(&line) {
                Ok(()) => self.last_write_error = None,
                Err(error) => {
                    let error = error.to_string();
                    if self.last_write_error.as_deref() != Some(error.as_str()) {
                        let _ = emergency_log(&format!("event log write failed: {error}"));
                    }
                    self.last_write_error = Some(error);
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows_service::service::ServiceDependency;

        #[test]
        fn queued_ticks_are_drained_without_hiding_control_commands() {
            let (sender, receiver) = mpsc::channel();
            sender.send(ControllerCommand::Tick).unwrap();
            sender.send(ControllerCommand::Tick).unwrap();
            sender.send(ControllerCommand::Disable).unwrap();
            assert_eq!(
                wait_for_command(Some(&receiver), Duration::from_secs(1)),
                ControllerCommand::Disable
            );
        }

        #[test]
        fn interactive_wake_enqueues_at_most_one_outstanding_tick() {
            let (sender, receiver) = mpsc::channel();
            let pending = AtomicBool::new(false);
            for _ in 0..100 {
                request_interactive_wake(&sender, &pending);
            }
            assert_eq!(receiver.try_recv(), Ok(ControllerCommand::Tick));
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ));

            pending.store(false, Ordering::Release);
            request_interactive_wake(&sender, &pending);
            assert_eq!(receiver.try_recv(), Ok(ControllerCommand::Tick));
        }

        fn prior_service_config() -> ServiceConfig {
            ServiceConfig {
                service_type: ServiceType::OWN_PROCESS,
                start_type: ServiceStartType::OnDemand,
                error_control: ServiceErrorControl::Severe,
                executable_path: PathBuf::from(
                    r#""C:\Old WinSched\winsched-service.exe" service --config "C:\ProgramData\WinSched\old config.toml""#,
                ),
                load_order_group: None,
                tag_id: 0,
                dependencies: vec![
                    ServiceDependency::Service(OsString::from("RpcSs")),
                    ServiceDependency::Group(OsString::from("NetworkProvider")),
                ],
                account_name: Some(OsString::from("LocalSystem")),
                display_name: OsString::from("Prior WinSched display name"),
            }
        }

        #[test]
        fn restore_mapping_preserves_raw_image_path_and_service_metadata() {
            let config = prior_service_config();
            let args = service_config_restore_args(&config).unwrap();

            assert_eq!(args[0], "config");
            assert_eq!(args[1], SERVICE_NAME);
            assert_eq!(
                args[2..10],
                [
                    "type=",
                    "own",
                    "start=",
                    "demand",
                    "error=",
                    "severe",
                    "binPath=",
                    r#""C:\Old WinSched\winsched-service.exe" service --config "C:\ProgramData\WinSched\old config.toml""#
                ]
            );
            assert_eq!(args[10], "depend=");
            assert_eq!(args[11], "RpcSs/+NetworkProvider");
            assert_eq!(args[12], "obj=");
            assert_eq!(args[13], "LocalSystem");
            assert_eq!(args[14], "DisplayName=");
            assert_eq!(args[15], "Prior WinSched display name");
        }

        #[test]
        fn restore_mapping_preserves_independent_delayed_auto_start_state() {
            let mut config = prior_service_config();
            config.start_type = ServiceStartType::AutoStart;

            let args = service_config_restore_args(&config).unwrap();
            assert!(!args.iter().any(|argument| argument == "start="));
            assert!(args.iter().any(|argument| argument == "binPath="));
        }

        #[test]
        fn restore_mapping_refuses_an_account_that_cannot_be_restored_without_a_password() {
            let mut config = prior_service_config();
            config.account_name = Some(OsString::from(r"CONTOSO\service-user"));

            assert_eq!(
                service_config_restore_args(&config),
                Err("the existing WinSched service must run as LocalSystem")
            );

            config.account_name = None;
            assert_eq!(
                service_config_restore_args(&config),
                Err("the existing WinSched service must run as LocalSystem")
            );
        }

        #[test]
        fn sc_snapshot_parsers_extract_description_and_complete_sddl() {
            let description = b"[SC] QueryServiceConfig2 SUCCESS\r\n\r\nSERVICE_NAME: WinSched\r\n        DESCRIPTION: Prior description: retained\r\n";
            let sddl = b"[SC] QueryServiceObjectSecurity SUCCESS\r\n\r\nO:SYG:SYD:(A;;CCLCSWLOCRRC;;;IU)S:(AU;FA;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;WD)\r\n";

            assert_eq!(
                parse_sc_description(description),
                Some(OsString::from("Prior description: retained"))
            );
            assert_eq!(
                parse_sc_sddl(sddl),
                Some(OsString::from(
                    "O:SYG:SYD:(A;;CCLCSWLOCRRC;;;IU)S:(AU;FA;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;WD)"
                ))
            );

            assert_eq!(parse_sc_description(b"SERVICE_NAME: WinSched\r\n"), None);
            assert_eq!(
                parse_sc_description(b"SERVICE_NAME: WinSched\r\n        DESCRIPTION:     \r\n"),
                Some(OsString::new())
            );
        }

        #[test]
        fn reload_receipt_is_persisted_for_success_and_rejection() {
            let directory = std::env::temp_dir().join(format!(
                "winsched-reload-receipt-{}-{}",
                std::process::id(),
                unix_time_ms()
            ));
            fs::create_dir_all(&directory).unwrap();
            let path = directory.join("status.json");
            let logging = LoggingConfig {
                enabled: false,
                max_file_size_mib: 2,
                retained_archives: 0,
            };
            let config = ControllerConfig {
                controller_mode: ControllerMode::Auto,
                logging,
                ..ControllerConfig::default()
            };
            let mut status = ControllerStatus::starting(
                42,
                true,
                &ControllerConfig::default(),
                SystemReservePlan::default(),
                2,
                unix_time_ms(),
            );
            let reserve_plan = SystemReservePlan::default();

            let event =
                apply_reload_status(&mut status, &config, &reserve_plan, ConfigReload::Reloaded)
                    .unwrap();
            persist_controller_status(Some(&path), &mut status).unwrap();
            let persisted =
                serde_json::from_slice::<ControllerStatus>(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(event["event"], "config_reloaded");
            assert_eq!(persisted.config_reload_sequence, 1);
            assert_eq!(persisted.config_reload_result, ConfigReloadResult::Reloaded);
            assert_eq!(persisted.config_reload_error, None);
            assert_eq!(persisted.applied_config_fingerprint, config.fingerprint());
            assert_eq!(persisted.applied_logging, logging);

            let event = apply_reload_status(
                &mut status,
                &config,
                &reserve_plan,
                ConfigReload::Rejected {
                    error: "injected invalid configuration".to_owned(),
                    fail_closed: true,
                },
            )
            .unwrap();
            // Runtime cleanup and enforcement may update the generic error independently.
            status.last_error = None;
            persist_controller_status(Some(&path), &mut status).unwrap();
            let persisted =
                serde_json::from_slice::<ControllerStatus>(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(event["event"], "config_rejected_fail_closed");
            assert_eq!(persisted.config_reload_sequence, 2);
            assert_eq!(persisted.config_reload_result, ConfigReloadResult::Rejected);
            assert_eq!(
                persisted.config_reload_error.as_deref(),
                Some("injected invalid configuration")
            );
            assert_eq!(persisted.applied_logging, logging);
            assert_eq!(persisted.last_error, None);

            fs::remove_dir_all(directory).unwrap();
        }

        #[test]
        fn invalid_reload_preserves_last_known_good_disabled_logging() {
            let directory = std::env::temp_dir().join(format!(
                "winsched-invalid-reload-{}-{}",
                std::process::id(),
                unix_time_ms()
            ));
            fs::create_dir_all(&directory).unwrap();
            let config_path = directory.join("winsched.toml");
            let log_path = directory.join("winsched.log");
            fs::write(
                &config_path,
                "schema_version = 2\nsample_interval_ms = 999\n",
            )
            .unwrap();
            let logging = LoggingConfig {
                enabled: false,
                max_file_size_mib: 3,
                retained_archives: 0,
            };
            let mut config = ControllerConfig {
                controller_mode: ControllerMode::Auto,
                logging,
                ..ControllerConfig::default()
            };
            let mut engine = PolicyEngine::new(config.policy).unwrap();
            let mut managed = BTreeMap::new();
            let mut modified = Some(UNIX_EPOCH);
            let mut logger = EventLogger::service(log_path.clone(), logging).unwrap();

            let reload = reload_config_if_changed(
                Some(&config_path),
                &mut modified,
                &mut config,
                &mut engine,
                &mut managed,
                None,
                &mut logger,
            )
            .unwrap();

            assert!(matches!(
                reload,
                ConfigReload::Rejected {
                    fail_closed: true,
                    ..
                }
            ));
            assert_eq!(config.controller_mode, ControllerMode::Observe);
            assert_eq!(config.logging, logging);
            assert!(!log_path.exists());
            fs::remove_dir_all(directory).unwrap();
        }

        #[test]
        fn failed_logging_reconfigure_rejects_reload_and_persists_prior_policy() {
            let directory = std::env::temp_dir().join(format!(
                "winsched-failed-log-reconfigure-{}-{}",
                std::process::id(),
                unix_time_ms()
            ));
            fs::create_dir_all(&directory).unwrap();
            let config_path = directory.join("winsched.toml");
            let status_path = directory.join("status.json");
            let blocking_file = directory.join("not-a-directory");
            fs::write(&blocking_file, "blocking file").unwrap();
            let log_path = blocking_file.join("winsched.log");
            let prior_logging = LoggingConfig {
                enabled: false,
                max_file_size_mib: 2,
                retained_archives: 0,
            };
            let mut config = ControllerConfig {
                controller_mode: ControllerMode::Auto,
                logging: prior_logging,
                ..ControllerConfig::default()
            };
            let updated = ControllerConfig {
                logging: LoggingConfig {
                    enabled: true,
                    max_file_size_mib: 1,
                    retained_archives: 1,
                },
                ..config.clone()
            };
            fs::write(&config_path, toml::to_string_pretty(&updated).unwrap()).unwrap();
            let mut engine = PolicyEngine::new(config.policy).unwrap();
            let mut managed = BTreeMap::new();
            let mut modified = Some(UNIX_EPOCH);
            let mut logger = EventLogger::service(log_path, prior_logging).unwrap();

            let reload = reload_config_if_changed(
                Some(&config_path),
                &mut modified,
                &mut config,
                &mut engine,
                &mut managed,
                None,
                &mut logger,
            )
            .unwrap();
            assert!(matches!(
                &reload,
                ConfigReload::Rejected {
                    fail_closed: false,
                    ..
                }
            ));
            assert_eq!(config.logging, prior_logging);

            let mut status = ControllerStatus::starting(
                42,
                true,
                &config,
                SystemReservePlan::default(),
                2,
                unix_time_ms(),
            );
            let event =
                apply_reload_status(&mut status, &config, &SystemReservePlan::default(), reload)
                    .unwrap();
            persist_controller_status(Some(&status_path), &mut status).unwrap();
            let persisted =
                serde_json::from_slice::<ControllerStatus>(&fs::read(&status_path).unwrap())
                    .unwrap();
            assert_eq!(event["event"], "config_rejected");
            assert_eq!(persisted.config_reload_sequence, 1);
            assert_eq!(persisted.config_reload_result, ConfigReloadResult::Rejected);
            assert_eq!(persisted.applied_config_fingerprint, config.fingerprint());
            assert_eq!(persisted.applied_logging, prior_logging);
            fs::remove_dir_all(directory).unwrap();
        }

        fn process(pid: u32, image: &str, cpu_time_100ns: u64) -> platform::ObservedProcess {
            platform::ObservedProcess {
                key: ProcessKey {
                    pid,
                    creation_time_100ns: u64::from(pid) * 10,
                },
                parent_pid: 0,
                session_id: Some(1),
                thread_count: 1,
                image_name: image.to_owned(),
                image_path: None,
                priority_class: None,
                cpu_time_100ns,
                default_cpu_set_ids: Vec::new(),
                current_domain: None,
                exclusion: None,
            }
        }

        fn interactive_state(
            session_id: u32,
            foreground_pid: Option<u32>,
            visible_pids: Vec<u32>,
            audible_pids: Vec<u32>,
        ) -> InteractiveActivityState {
            InteractiveActivityState {
                schema_version: winsched_control::INTERACTIVE_STATE_SCHEMA_VERSION,
                session_id,
                source_pid: 900,
                source_creation_time_100ns: 9_000,
                window_probe_available: true,
                audio_probe_available: true,
                foreground_pid,
                visible_pids,
                audible_pids,
                updated_at_unix_ms: unix_time_ms(),
            }
        }

        fn background_config() -> ControllerConfig {
            ControllerConfig::from_toml(
                r#"
schema_version = 4
controller_mode = "auto"

[background_efficiency]
enabled = true
eco_qos_enabled = true
memory_priority_enabled = true

[[rules]]
image = "worker.exe"
mode = "auto"
profile = "background"
"#,
            )
            .unwrap()
        }

        #[test]
        fn background_guard_wakes_quickly_without_accelerating_cpu_policy() {
            let mut config = background_config();
            config.sample_interval_ms = 60_000;
            let runtime = RuntimeState::for_controller_mode(ControllerMode::Auto);
            let started = Instant::now();

            let guarded = controller_wait_interval(&config, &runtime, &BTreeMap::new(), started);
            assert!(guarded <= BACKGROUND_SAFETY_INTERVAL);

            config.background_efficiency.enabled = false;
            let unguarded = controller_wait_interval(&config, &runtime, &BTreeMap::new(), started);
            assert!(unguarded > Duration::from_secs(59));

            let key = ProcessKey {
                pid: 42,
                creation_time_100ns: 420,
            };
            let state = ProcessEfficiencyState {
                eco_qos: ProcessEcoQosState::Unset,
                memory_priority: ProcessMemoryPriority::Normal,
            };
            let owned = ManagedBackgroundProcess {
                key,
                original: state,
                applied: state,
                ownership: ProcessEfficiencyOwnership {
                    eco_qos: false,
                    memory_priority: true,
                },
                pending: None,
                pending_ownership: None,
                blocked_by_external_override: ProcessEfficiencyOwnership::default(),
            };
            let managed = BTreeMap::from([(key, owned)]);
            assert!(
                controller_wait_interval(&config, &runtime, &managed, started)
                    <= BACKGROUND_SAFETY_INTERVAL
            );
        }

        struct ChildGuard(std::process::Child);

        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        fn background_child() -> ChildGuard {
            ChildGuard(
                std::process::Command::new("cmd.exe")
                    .args(["/d", "/c", "ping.exe -n 30 127.0.0.1 >nul"])
                    .spawn()
                    .expect("background child must start"),
            )
        }

        #[test]
        fn interactive_guards_fail_closed_and_protect_the_exact_rule_cohort() {
            let config = background_config();
            let mut foreground = process(10, "worker.exe", 100);
            foreground.session_id = Some(2);
            let mut helper = process(20, "worker.exe", 100);
            helper.session_id = Some(2);
            helper.parent_pid = foreground.key.pid;
            let processes = vec![foreground, helper];

            let missing = interactive_sessions(&[], unix_time_ms());
            assert_eq!(
                background_protection(&config, &processes[1], &processes, &missing),
                Some(BackgroundProtection::ProbeUnavailable)
            );

            let state = interactive_state(2, Some(10), vec![10], Vec::new());
            let sessions = interactive_sessions(&[state], unix_time_ms());
            assert_eq!(
                background_protection(&config, &processes[1], &processes, &sessions),
                Some(BackgroundProtection::Foreground)
            );

            let clear = interactive_state(2, Some(99), vec![99], Vec::new());
            let sessions = interactive_sessions(&[clear], unix_time_ms());
            assert_eq!(
                background_protection(&config, &processes[1], &processes, &sessions),
                None
            );
        }

        #[test]
        fn incomplete_audio_probe_blocks_background_policy() {
            let config = background_config();
            let mut worker = process(10, "worker.exe", 100);
            worker.session_id = Some(2);
            let mut state = interactive_state(2, Some(99), vec![99], Vec::new());
            state.audio_probe_available = false;
            let sessions = interactive_sessions(&[state], unix_time_ms());
            let processes = vec![worker];
            assert_eq!(
                background_protection(&config, &processes[0], &processes, &sessions),
                Some(BackgroundProtection::ProbeUnavailable)
            );
        }

        #[test]
        fn sensor_status_counts_only_ready_required_sessions() {
            let config = background_config();
            let mut required = process(10, "worker.exe", 100);
            required.session_id = Some(1);
            let unrelated = interactive_state(2, Some(99), vec![99], Vec::new());
            let mut streaks = BTreeMap::new();
            let mut managed = BTreeMap::new();
            let mut logger = EventLogger::console();

            let report = reconcile_background_efficiency(
                &config,
                true,
                &[required],
                &[unrelated],
                Some(false),
                true,
                &mut streaks,
                &mut managed,
                None,
                &mut logger,
            )
            .unwrap();
            assert_eq!(report.status.required_probe_sessions, 1);
            assert_eq!(report.status.interactive_probe_sessions, 0);
            assert_eq!(report.status.protected_processes, 1);
        }

        #[test]
        fn memory_pressure_only_strengthens_owned_background_memory_priority() {
            let config = background_config();
            let original = ProcessEfficiencyState {
                eco_qos: ProcessEcoQosState::Unset,
                memory_priority: ProcessMemoryPriority::Normal,
            };
            assert_eq!(
                desired_background_state(&config, original, false),
                ProcessEfficiencyState {
                    eco_qos: ProcessEcoQosState::Enabled,
                    memory_priority: ProcessMemoryPriority::BelowNormal,
                }
            );
            assert_eq!(
                desired_background_state(&config, original, true).memory_priority,
                ProcessMemoryPriority::Low
            );
        }

        #[test]
        fn background_memory_priority_never_raises_an_existing_lower_priority() {
            let config = background_config();
            let cases = [
                (
                    ProcessMemoryPriority::VeryLow,
                    ProcessMemoryPriority::VeryLow,
                    ProcessMemoryPriority::VeryLow,
                ),
                (
                    ProcessMemoryPriority::Low,
                    ProcessMemoryPriority::Low,
                    ProcessMemoryPriority::Low,
                ),
                (
                    ProcessMemoryPriority::Medium,
                    ProcessMemoryPriority::Medium,
                    ProcessMemoryPriority::Low,
                ),
                (
                    ProcessMemoryPriority::BelowNormal,
                    ProcessMemoryPriority::BelowNormal,
                    ProcessMemoryPriority::Low,
                ),
                (
                    ProcessMemoryPriority::Normal,
                    ProcessMemoryPriority::BelowNormal,
                    ProcessMemoryPriority::Low,
                ),
            ];
            for (original_priority, normal_pressure, low_pressure) in cases {
                let original = ProcessEfficiencyState {
                    eco_qos: ProcessEcoQosState::Unset,
                    memory_priority: original_priority,
                };
                assert_eq!(
                    desired_background_state(&config, original, false).memory_priority,
                    normal_pressure
                );
                assert_eq!(
                    desired_background_state(&config, original, true).memory_priority,
                    low_pressure
                );
            }
        }

        #[test]
        #[allow(clippy::too_many_lines)]
        fn background_reconciler_applies_then_restores_a_real_owned_child() {
            let child = background_child();
            let pid = child.0.id();
            let topology = platform::system_topology().unwrap();
            let observed = platform::observe_processes(&topology)
                .unwrap()
                .into_iter()
                .find(|process| process.key.pid == pid)
                .expect("child process must be observable");
            let ambient = platform::query_process_efficiency_key(observed.key).unwrap();
            let original = ProcessEfficiencyState {
                memory_priority: ProcessMemoryPriority::Normal,
                ..ambient
            };
            if ambient != original {
                platform::apply_process_efficiency_key(
                    observed.key,
                    ambient,
                    original,
                    ProcessEfficiencyOwnership {
                        eco_qos: false,
                        memory_priority: true,
                    },
                )
                .unwrap();
            }
            let mut config = ControllerConfig {
                controller_mode: ControllerMode::Auto,
                rules: vec![winsched_config::ProcessRule {
                    image: observed.image_name.clone(),
                    mode: winsched_config::RuleMode::Auto,
                    profile: WorkloadProfile::Background,
                    group: None,
                    llc: None,
                }],
                ..background_config()
            };
            config.policy.max_mutations_per_evaluation = 1;
            let state_path = std::env::temp_dir().join(format!(
                "winsched-background-reconcile-{}-{}.json",
                std::process::id(),
                unix_time_ms()
            ));
            let clear = interactive_state(
                observed.session_id.unwrap(),
                Some(u32::MAX),
                Vec::new(),
                Vec::new(),
            );
            let processes = vec![observed.clone()];
            let mut streaks = BTreeMap::new();
            let mut managed = BTreeMap::new();
            let mut logger = EventLogger::console();

            reconcile_background_efficiency(
                &config,
                true,
                &processes,
                std::slice::from_ref(&clear),
                Some(false),
                true,
                &mut streaks,
                &mut managed,
                Some(&state_path),
                &mut logger,
            )
            .unwrap();
            assert!(managed.is_empty());
            let applied = reconcile_background_efficiency(
                &config,
                true,
                &processes,
                std::slice::from_ref(&clear),
                Some(false),
                true,
                &mut streaks,
                &mut managed,
                Some(&state_path),
                &mut logger,
            )
            .unwrap();
            assert_eq!(applied.status.managed_processes, 1);
            assert_eq!(
                platform::query_process_efficiency_key(observed.key)
                    .unwrap()
                    .eco_qos,
                ProcessEcoQosState::Enabled
            );

            let visible = interactive_state(
                observed.session_id.unwrap(),
                Some(pid),
                vec![pid],
                Vec::new(),
            );
            let restored = reconcile_background_efficiency(
                &config,
                true,
                &processes,
                &[visible],
                Some(false),
                true,
                &mut streaks,
                &mut managed,
                Some(&state_path),
                &mut logger,
            )
            .unwrap();
            assert_eq!(restored.status.protected_processes, 1);
            assert!(managed.is_empty());
            assert_eq!(
                platform::query_process_efficiency_key(observed.key).unwrap(),
                original
            );
            if ambient != original {
                platform::apply_process_efficiency_key(
                    observed.key,
                    original,
                    ambient,
                    ProcessEfficiencyOwnership {
                        eco_qos: false,
                        memory_priority: true,
                    },
                )
                .unwrap();
            }
            fs::remove_file(state_path).unwrap();
        }

        #[test]
        #[allow(clippy::too_many_lines)]
        fn external_memory_override_survives_visible_veto_and_eco_reapply() {
            let child = background_child();
            let pid = child.0.id();
            let topology = platform::system_topology().unwrap();
            let observed = platform::observe_processes(&topology)
                .unwrap()
                .into_iter()
                .find(|process| process.key.pid == pid)
                .expect("child process must be observable");
            let ambient = platform::query_process_efficiency_key(observed.key).unwrap();
            let original = ProcessEfficiencyState {
                memory_priority: ProcessMemoryPriority::Normal,
                ..ambient
            };
            if ambient != original {
                platform::apply_process_efficiency_key(
                    observed.key,
                    ambient,
                    original,
                    ProcessEfficiencyOwnership {
                        eco_qos: false,
                        memory_priority: true,
                    },
                )
                .unwrap();
            }
            let config = ControllerConfig {
                controller_mode: ControllerMode::Auto,
                rules: vec![winsched_config::ProcessRule {
                    image: observed.image_name.clone(),
                    mode: winsched_config::RuleMode::Auto,
                    profile: WorkloadProfile::Background,
                    group: None,
                    llc: None,
                }],
                ..background_config()
            };
            let state_path = std::env::temp_dir().join(format!(
                "winsched-background-external-veto-{}-{}.json",
                std::process::id(),
                unix_time_ms()
            ));
            let clear = interactive_state(
                observed.session_id.unwrap(),
                Some(u32::MAX),
                Vec::new(),
                Vec::new(),
            );
            let processes = vec![observed.clone()];
            let mut streaks = BTreeMap::new();
            let mut managed = BTreeMap::new();
            let mut logger = EventLogger::console();
            for _ in 0..2 {
                reconcile_background_efficiency(
                    &config,
                    true,
                    &processes,
                    std::slice::from_ref(&clear),
                    Some(false),
                    true,
                    &mut streaks,
                    &mut managed,
                    Some(&state_path),
                    &mut logger,
                )
                .unwrap();
            }
            let applied = platform::query_process_efficiency_key(observed.key).unwrap();
            let external = ProcessEfficiencyState {
                memory_priority: ProcessMemoryPriority::Medium,
                ..applied
            };
            platform::apply_process_efficiency_key(
                observed.key,
                applied,
                external,
                ProcessEfficiencyOwnership {
                    eco_qos: false,
                    memory_priority: true,
                },
            )
            .unwrap();
            reconcile_background_efficiency(
                &config,
                true,
                &processes,
                std::slice::from_ref(&clear),
                Some(false),
                true,
                &mut streaks,
                &mut managed,
                Some(&state_path),
                &mut logger,
            )
            .unwrap();
            let visible = interactive_state(
                observed.session_id.unwrap(),
                Some(pid),
                vec![pid],
                Vec::new(),
            );
            reconcile_background_efficiency(
                &config,
                true,
                &processes,
                &[visible],
                Some(false),
                true,
                &mut streaks,
                &mut managed,
                Some(&state_path),
                &mut logger,
            )
            .unwrap();
            let blocked = managed.get(&observed.key).unwrap();
            assert!(blocked.ownership.is_empty());
            assert!(blocked.blocked_by_external_override.memory_priority);
            let restored = platform::query_process_efficiency_key(observed.key).unwrap();
            assert_eq!(restored.eco_qos, original.eco_qos);
            assert_eq!(restored.memory_priority, ProcessMemoryPriority::Medium);

            for expected_eco in [original.eco_qos, ProcessEcoQosState::Enabled] {
                reconcile_background_efficiency(
                    &config,
                    true,
                    &processes,
                    std::slice::from_ref(&clear),
                    Some(false),
                    true,
                    &mut streaks,
                    &mut managed,
                    Some(&state_path),
                    &mut logger,
                )
                .unwrap();
                let state = platform::query_process_efficiency_key(observed.key).unwrap();
                assert_eq!(state.eco_qos, expected_eco);
                assert_eq!(state.memory_priority, ProcessMemoryPriority::Medium);
            }
            cleanup_background(&mut logger, &mut managed, Some(&state_path)).unwrap();
            let current = platform::query_process_efficiency_key(observed.key).unwrap();
            if current != ambient {
                platform::apply_process_efficiency_key(
                    observed.key,
                    current,
                    ambient,
                    ProcessEfficiencyOwnership {
                        eco_qos: false,
                        memory_priority: true,
                    },
                )
                .unwrap();
            }
            fs::remove_file(state_path).unwrap();
        }

        fn observation_topology() -> Topology {
            Topology::new(vec![
                winsched_core::CpuSet {
                    id: 256,
                    group: 0,
                    logical_processor_index: 0,
                    core_index: 0,
                    last_level_cache_index: 1,
                    numa_node_index: 0,
                    efficiency_class: 0,
                    scheduling_class: 1,
                    flags: winsched_core::CpuSetFlags::default(),
                    allocation_tag: 0,
                },
                winsched_core::CpuSet {
                    id: 257,
                    group: 0,
                    logical_processor_index: 1,
                    core_index: 0,
                    last_level_cache_index: 1,
                    numa_node_index: 0,
                    efficiency_class: 0,
                    scheduling_class: 1,
                    flags: winsched_core::CpuSetFlags::default(),
                    allocation_tag: 0,
                },
            ])
            .unwrap()
        }

        fn profile_topology() -> Topology {
            let mut cpu_sets = Vec::new();
            for llc in 0u8..4 {
                for core_in_llc in 0u8..2 {
                    let core_index = llc * 4 + core_in_llc * 2;
                    for sibling in 0u8..2 {
                        let logical = core_index + sibling;
                        cpu_sets.push(winsched_core::CpuSet {
                            id: 400 + u32::from(logical),
                            group: 0,
                            logical_processor_index: logical,
                            core_index,
                            last_level_cache_index: llc * 4,
                            numa_node_index: 0,
                            efficiency_class: 0,
                            scheduling_class: llc * 2 + core_in_llc,
                            flags: winsched_core::CpuSetFlags::default(),
                            allocation_tag: 0,
                        });
                    }
                }
            }
            Topology::new(cpu_sets).unwrap()
        }

        fn managed_placement(
            anchor_domain: winsched_core::LlcDomainKey,
            cpu_set_ids: &[u32],
        ) -> ManagedPlacement {
            ManagedPlacement {
                anchor_domain,
                cpu_set_ids: cpu_set_ids.to_vec(),
            }
        }

        #[test]
        fn process_ranking_prefers_largest_cpu_delta() {
            let config = ControllerConfig::from_toml(
                r#"
schema_version = 1
controller_mode = "auto"
[[rules]]
image = "a.exe"
[[rules]]
image = "b.exe"
"#,
            )
            .unwrap();
            let processes = vec![process(10, "a.exe", 120), process(20, "b.exe", 250)];
            let mut previous = BTreeMap::from([(processes[0].key, 100), (processes[1].key, 100)]);

            let ranked = build_ranked_observations(
                &config,
                &BTreeMap::new(),
                &processes,
                &observation_topology(),
                28,
                &mut previous,
                1_000,
            );
            assert_eq!(ranked[0].key.pid, 20);
            assert_eq!(ranked[1].key.pid, 10);
        }

        #[test]
        fn decision_summary_is_compact_and_user_facing() {
            let observed = process(20, "game.exe", 250);
            let decision = PolicyDecision {
                process: observed.key,
                action: PolicyAction::Keep {
                    domain: Some(winsched_core::LlcDomainKey {
                        group: 0,
                        last_level_cache_index: 2,
                    }),
                },
                reason: DecisionReason::RateLimited,
                enforce: false,
            };
            assert_eq!(
                decision_summary(&[observed], &decision),
                "game.exe (PID 20): kept on LLC 0:2; mutation rate limit"
            );
        }

        #[test]
        fn implicit_scope_requires_activity_but_explicit_rules_do_not() {
            let implicit = ControllerConfig::from_toml(
                r#"
schema_version = 1
controller_mode = "auto"
all_user_processes = true
minimum_process_utilization_bps = 500
"#,
            )
            .unwrap();
            let processes = vec![
                process(10, "idle.exe", 100),
                process(20, "busy.exe", 1_000_000),
            ];
            let mut previous = BTreeMap::from([(processes[0].key, 100), (processes[1].key, 0)]);
            let ranked = build_ranked_observations(
                &implicit,
                &BTreeMap::new(),
                &processes,
                &observation_topology(),
                28,
                &mut previous,
                1_000,
            );
            assert_eq!(ranked.len(), 1);
            assert_eq!(ranked[0].key.pid, 20);

            let explicit = ControllerConfig::from_toml(
                r#"
schema_version = 1
controller_mode = "auto"
minimum_process_utilization_bps = 500
[[rules]]
image = "idle.exe"
"#,
            )
            .unwrap();
            let mut previous = BTreeMap::from([(processes[0].key, 100)]);
            let ranked = build_ranked_observations(
                &explicit,
                &BTreeMap::new(),
                &processes[..1],
                &observation_topology(),
                28,
                &mut previous,
                1_000,
            );
            assert_eq!(ranked.len(), 1);
            assert_eq!(ranked[0].key.pid, 10);
        }

        #[test]
        fn recovered_process_removed_from_scope_is_cleared() {
            let config = ControllerConfig::from_toml(
                r#"
schema_version = 1
controller_mode = "auto"
all_user_processes = false
"#,
            )
            .unwrap();
            let mut observed = process(10, "old-rule.exe", 100);
            observed.current_domain = Some(winsched_core::LlcDomainKey {
                group: 0,
                last_level_cache_index: 1,
            });
            observed.default_cpu_set_ids = vec![256];
            let managed = BTreeMap::from([(
                observed.key,
                managed_placement(
                    winsched_core::LlcDomainKey {
                        group: 0,
                        last_level_cache_index: 1,
                    },
                    &[256],
                ),
            )]);
            let mut previous = BTreeMap::new();
            let ranked = build_ranked_observations(
                &config,
                &managed,
                &[observed],
                &observation_topology(),
                28,
                &mut previous,
                1_000,
            );
            assert_eq!(ranked.len(), 1);
            assert_eq!(ranked[0].mode, winsched_core::adaptive::PlacementMode::Off);
            assert_eq!(
                ranked[0].enforcement,
                winsched_core::adaptive::EnforcementMode::Apply
            );
            assert_eq!(ranked[0].assignment_origin, AssignmentOrigin::Managed);
        }

        #[test]
        fn managed_process_is_refreshed_when_a_cpu_set_becomes_reserved() {
            let config = ControllerConfig::from_toml(
                r#"
schema_version = 3
controller_mode = "auto"
[[rules]]
image = "interactive.exe"
mode = "sticky"
profile = "interactive"
"#,
            )
            .unwrap();
            let mut observed = process(10, "interactive.exe", 100);
            observed.current_domain = Some(winsched_core::LlcDomainKey {
                group: 0,
                last_level_cache_index: 1,
            });
            observed.default_cpu_set_ids = vec![256, 257];
            let managed = BTreeMap::from([(
                observed.key,
                managed_placement(
                    winsched_core::LlcDomainKey {
                        group: 0,
                        last_level_cache_index: 1,
                    },
                    &[256, 257],
                ),
            )]);
            let placement =
                observation_topology().excluding_reserved_cpu_sets(&SystemReservePlan {
                    reserved_cpu_set_ids: vec![256],
                    ..SystemReservePlan::default()
                });
            let mut previous = BTreeMap::new();

            let ranked = build_ranked_observations(
                &config,
                &managed,
                &[observed],
                &placement,
                28,
                &mut previous,
                1_000,
            );

            assert_eq!(ranked.len(), 1);
            assert!(ranked[0].refresh_required);
            assert_eq!(ranked[0].assignment_origin, AssignmentOrigin::Managed);
        }

        #[test]
        fn managed_process_leaves_a_domain_that_becomes_fully_reserved() {
            let config = ControllerConfig::from_toml(
                r#"
schema_version = 3
controller_mode = "auto"
[[rules]]
image = "interactive.exe"
mode = "sticky"
profile = "interactive"
"#,
            )
            .unwrap();
            let mut observed = process(10, "interactive.exe", 100);
            let anchor = winsched_core::LlcDomainKey {
                group: 0,
                last_level_cache_index: 1,
            };
            observed.current_domain = Some(anchor);
            observed.default_cpu_set_ids = vec![256, 257];
            let managed = BTreeMap::from([(observed.key, managed_placement(anchor, &[256, 257]))]);
            let placement =
                observation_topology().excluding_reserved_cpu_sets(&SystemReservePlan {
                    reserved_cpu_set_ids: vec![256, 257],
                    ..SystemReservePlan::default()
                });
            let mut previous = BTreeMap::new();

            let ranked = build_ranked_observations(
                &config,
                &managed,
                &[observed],
                &placement,
                28,
                &mut previous,
                1_000,
            );

            assert_eq!(ranked.len(), 1);
            assert_eq!(ranked[0].current_domain, None);
            assert!(!ranked[0].refresh_required);
        }

        #[test]
        fn memory_profile_uses_one_smt_thread_across_the_requested_core_width() {
            let config = ControllerConfig::from_toml(
                r#"
schema_version = 3
controller_mode = "auto"
[[rules]]
image = "memory.exe"
mode = "sticky"
profile = "memory"
"#,
            )
            .unwrap();
            let observed = process(10, "memory.exe", 100);
            let mut previous = BTreeMap::new();

            let ranked = build_ranked_observations(
                &config,
                &BTreeMap::new(),
                &[observed],
                &profile_topology(),
                4,
                &mut previous,
                1_000,
            );

            let partition = ranked[0].preferred_partition.as_ref().unwrap();
            assert_eq!(partition.physical_cores.len(), 4);
            assert_eq!(partition.cpu_set_ids.len(), 4);
            assert_eq!(partition.llc_domains.len(), 4);
            assert!(!partition.uses_smt);
        }

        #[test]
        fn compute_profile_uses_both_smt_threads_on_every_available_core() {
            let config = ControllerConfig::from_toml(
                r#"
schema_version = 3
controller_mode = "auto"
[[rules]]
image = "compute.exe"
mode = "sticky"
profile = "compute"
"#,
            )
            .unwrap();
            let observed = process(10, "compute.exe", 100);
            let mut previous = BTreeMap::new();

            let ranked = build_ranked_observations(
                &config,
                &BTreeMap::new(),
                &[observed],
                &profile_topology(),
                4,
                &mut previous,
                1_000,
            );

            let partition = ranked[0].preferred_partition.as_ref().unwrap();
            assert_eq!(partition.physical_cores.len(), 8);
            assert_eq!(partition.cpu_set_ids.len(), 16);
            assert!(partition.uses_smt);
        }

        #[test]
        fn adaptive_memory_width_is_clamped_to_the_available_placement_topology() {
            let mut config = ControllerConfig::default();
            config.responsiveness.enabled = true;
            config.responsiveness.memory.minimum_physical_cores = 8;
            config.responsiveness.memory.maximum_physical_cores = 28;
            let topology = observation_topology();
            let placement = topology.excluding_reserved_cpu_sets(&SystemReservePlan {
                reserved_cpu_set_ids: vec![256, 257],
                ..SystemReservePlan::default()
            });

            let width = adaptive_width_config(&config, &placement);
            assert_eq!(width.minimum_physical_cores, 0);
            assert_eq!(width.maximum_physical_cores, 0);
        }

        #[test]
        fn managed_state_round_trips_and_replaces_atomically() {
            let path = std::env::temp_dir().join(format!(
                "winsched-managed-state-{}-{}.json",
                std::process::id(),
                unix_time_ms()
            ));
            let key = ProcessKey {
                pid: 77,
                creation_time_100ns: 123,
            };
            let domain = winsched_core::LlcDomainKey {
                group: 0,
                last_level_cache_index: 2,
            };
            let placement = managed_placement(domain, &[300, 301]);
            let managed = BTreeMap::from([(key, placement)]);

            persist_managed_state(Some(&path), &managed).unwrap();
            assert_eq!(load_managed_state(&path).unwrap(), managed);
            persist_managed_state(Some(&path), &BTreeMap::new()).unwrap();
            assert!(load_managed_state(&path).unwrap().is_empty());

            fs::remove_file(&path).unwrap();
        }

        #[test]
        fn background_journal_round_trips_pending_and_external_block_state() {
            let path = std::env::temp_dir().join(format!(
                "winsched-background-state-{}-{}.json",
                std::process::id(),
                unix_time_ms()
            ));
            let key = ProcessKey {
                pid: 91,
                creation_time_100ns: 910,
            };
            let original = ProcessEfficiencyState {
                eco_qos: ProcessEcoQosState::Unset,
                memory_priority: ProcessMemoryPriority::Normal,
            };
            let desired = ProcessEfficiencyState {
                eco_qos: ProcessEcoQosState::Enabled,
                memory_priority: ProcessMemoryPriority::Normal,
            };
            let record = ManagedBackgroundProcess {
                key,
                original,
                applied: original,
                ownership: ProcessEfficiencyOwnership::between(original, desired),
                pending: Some(desired),
                pending_ownership: Some(ProcessEfficiencyOwnership::between(original, desired)),
                blocked_by_external_override: ProcessEfficiencyOwnership {
                    eco_qos: false,
                    memory_priority: true,
                },
            };
            let managed = BTreeMap::from([(key, record)]);

            persist_background_state(Some(&path), &managed).unwrap();
            assert_eq!(load_background_state(&path).unwrap(), managed);
            persist_background_state(Some(&path), &BTreeMap::new()).unwrap();
            assert!(load_background_state(&path).unwrap().is_empty());
            fs::remove_file(path).unwrap();
        }

        #[test]
        fn partial_restore_releases_the_successful_property_only() {
            let key = ProcessKey {
                pid: 93,
                creation_time_100ns: 930,
            };
            let original = ProcessEfficiencyState {
                eco_qos: ProcessEcoQosState::Unset,
                memory_priority: ProcessMemoryPriority::Normal,
            };
            let applied = ProcessEfficiencyState {
                eco_qos: ProcessEcoQosState::Enabled,
                memory_priority: ProcessMemoryPriority::Low,
            };
            let mut record = ManagedBackgroundProcess {
                key,
                original,
                applied,
                ownership: ProcessEfficiencyOwnership {
                    eco_qos: true,
                    memory_priority: true,
                },
                pending: None,
                pending_ownership: None,
                blocked_by_external_override: ProcessEfficiencyOwnership::default(),
            };
            let report = platform::EfficiencyMutationReport {
                operation: "restore_background_efficiency".to_owned(),
                pid: key.pid,
                committed: false,
                previous: applied,
                requested: ProcessEfficiencyState {
                    eco_qos: original.eco_qos,
                    memory_priority: applied.memory_priority,
                },
                observed: ProcessEfficiencyState {
                    eco_qos: original.eco_qos,
                    memory_priority: applied.memory_priority,
                },
                eco_qos_changed: true,
                memory_priority_changed: false,
                external_eco_qos_preserved: false,
                external_memory_priority_preserved: false,
                unrestored_ownership: ProcessEfficiencyOwnership {
                    eco_qos: false,
                    memory_priority: true,
                },
                property_errors: vec!["memory-priority restore failed".to_owned()],
            };

            apply_restore_report_to_record(&mut record, &report);
            assert_eq!(
                record.ownership,
                ProcessEfficiencyOwnership {
                    eco_qos: false,
                    memory_priority: true,
                }
            );
            assert_eq!(record.original.eco_qos, ProcessEcoQosState::Unset);
            assert_eq!(
                record.original.memory_priority,
                ProcessMemoryPriority::Normal
            );
            assert_eq!(record.applied.memory_priority, ProcessMemoryPriority::Low);
        }

        #[test]
        fn interrupted_legacy_atomic_write_recovers_the_backup_file() {
            let path = std::env::temp_dir().join(format!(
                "winsched-legacy-backup-{}-{}.json",
                std::process::id(),
                unix_time_ms()
            ));
            let backup = path.with_extension("bak");
            fs::write(&backup, b"durable journal").unwrap();

            assert_eq!(
                read_state_with_legacy_backup(&path).unwrap(),
                Some(b"durable journal".to_vec())
            );
            assert_eq!(fs::read(&path).unwrap(), b"durable journal");
            assert!(!backup.exists());
            fs::remove_file(path).unwrap();
        }

        #[test]
        fn schema_one_background_journal_migrates_to_property_ownership() {
            let path = std::env::temp_dir().join(format!(
                "winsched-background-v1-{}-{}.json",
                std::process::id(),
                unix_time_ms()
            ));
            fs::write(
                &path,
                serde_json::to_vec_pretty(&json!({
                    "schema_version": 1,
                    "processes": [
                        {
                            "key": { "pid": 91, "creation_time_100ns": 910 },
                            "original": { "eco_qos": "unset", "memory_priority": "normal" },
                            "applied": { "eco_qos": "enabled", "memory_priority": "below_normal" },
                            "pending": { "eco_qos": "enabled", "memory_priority": "low" },
                            "blocked_by_external_override": false
                        },
                        {
                            "key": { "pid": 92, "creation_time_100ns": 920 },
                            "original": { "eco_qos": "unset", "memory_priority": "normal" },
                            "applied": { "eco_qos": "enabled", "memory_priority": "below_normal" },
                            "pending": { "eco_qos": "enabled", "memory_priority": "low" },
                            "blocked_by_external_override": true
                        }
                    ]
                }))
                .unwrap(),
            )
            .unwrap();

            let migrated = load_background_state(&path).unwrap();
            let record = migrated
                .get(&ProcessKey {
                    pid: 91,
                    creation_time_100ns: 910,
                })
                .unwrap();
            assert_eq!(
                record.ownership,
                ProcessEfficiencyOwnership {
                    eco_qos: true,
                    memory_priority: true,
                }
            );
            assert_eq!(record.pending_ownership, Some(record.ownership));
            assert!(record.blocked_by_external_override.is_empty());
            let blocked = migrated
                .get(&ProcessKey {
                    pid: 92,
                    creation_time_100ns: 920,
                })
                .unwrap();
            assert!(blocked.ownership.is_empty());
            assert!(blocked.pending.is_none());
            assert!(blocked.pending_ownership.is_none());
            assert_eq!(
                blocked.blocked_by_external_override,
                ProcessEfficiencyOwnership {
                    eco_qos: true,
                    memory_priority: true,
                }
            );
            fs::remove_file(path).unwrap();
        }

        #[test]
        fn installer_detects_when_the_source_is_the_installed_config() {
            let directory = std::env::temp_dir().join(format!(
                "winsched-install-config-identity-{}-{}",
                std::process::id(),
                unix_time_ms()
            ));
            fs::create_dir_all(&directory).unwrap();
            let installed = directory.join("winsched.toml");
            let external = directory.join("external.toml");
            fs::write(&installed, "schema_version = 1\n").unwrap();
            fs::write(&external, "schema_version = 3\n").unwrap();

            assert!(paths_refer_to_same_file(&installed, &installed).unwrap());
            assert!(!paths_refer_to_same_file(&external, &installed).unwrap());
            assert!(
                !paths_refer_to_same_file(&directory.join("missing.toml"), &installed).unwrap()
            );
            fs::remove_dir_all(directory).unwrap();
        }

        #[test]
        fn schema_one_managed_state_migrates_to_exact_partition_records() {
            let path = std::env::temp_dir().join(format!(
                "winsched-managed-state-migration-{}-{}.json",
                std::process::id(),
                unix_time_ms()
            ));
            let key = ProcessKey {
                pid: 78,
                creation_time_100ns: 124,
            };
            let domain = winsched_core::LlcDomainKey {
                group: 0,
                last_level_cache_index: 2,
            };
            fs::write(
                &path,
                serde_json::to_vec_pretty(&json!({
                    "schema_version": 1,
                    "processes": [{
                        "key": key,
                        "domain": domain,
                    }],
                }))
                .unwrap(),
            )
            .unwrap();

            let managed = load_managed_state(&path).unwrap();
            assert_eq!(managed.get(&key), Some(&managed_placement(domain, &[])));
            persist_managed_state(Some(&path), &managed).unwrap();
            let persisted = fs::read_to_string(&path).unwrap();
            assert!(persisted.contains("\"schema_version\": 2"));
            assert!(persisted.contains("\"cpu_set_ids\""));
            fs::remove_file(path).unwrap();
        }

        #[test]
        fn cleanup_retains_failed_assignments_in_the_journal() {
            let path = std::env::temp_dir().join(format!(
                "winsched-cleanup-state-{}-{}.json",
                std::process::id(),
                unix_time_ms()
            ));
            let first = ProcessKey {
                pid: 70,
                creation_time_100ns: 700,
            };
            let second = ProcessKey {
                pid: 80,
                creation_time_100ns: 800,
            };
            let domain = winsched_core::LlcDomainKey {
                group: 0,
                last_level_cache_index: 1,
            };
            let placement = managed_placement(domain, &[300, 301]);
            let mut managed =
                BTreeMap::from([(first, placement.clone()), (second, placement.clone())]);
            let mut logger = EventLogger::console();

            let report = cleanup_managed_with(&mut logger, &mut managed, Some(&path), |process| {
                if process == first {
                    Ok(())
                } else {
                    Err("injected clear failure".to_owned())
                }
            })
            .unwrap();

            assert_eq!(
                report,
                CleanupReport {
                    attempted: 2,
                    cleared: 1,
                    failed: 1,
                }
            );
            assert_eq!(managed, BTreeMap::from([(second, placement)]));
            assert_eq!(load_managed_state(&path).unwrap(), managed);
            fs::remove_file(path).unwrap();
        }

        #[test]
        fn cleanup_persistence_failure_keeps_uncleared_ownership_in_memory() {
            let missing_parent = std::env::temp_dir().join(format!(
                "winsched-missing-parent-{}-{}",
                std::process::id(),
                unix_time_ms()
            ));
            let path = missing_parent.join("managed-state.json");
            let key = ProcessKey {
                pid: 90,
                creation_time_100ns: 900,
            };
            let domain = winsched_core::LlcDomainKey {
                group: 0,
                last_level_cache_index: 1,
            };
            let placement = managed_placement(domain, &[300, 301]);
            let mut managed = BTreeMap::from([(key, placement.clone())]);
            let mut logger = EventLogger::console();

            let result = cleanup_managed_with(&mut logger, &mut managed, Some(&path), |_| {
                Err("injected clear failure".to_owned())
            });
            assert!(matches!(result, Err(ServiceError::Io(_))));
            assert_eq!(managed, BTreeMap::from([(key, placement)]));
        }

        #[test]
        fn runtime_state_defaults_from_mode_and_persists_user_choice() {
            let path = std::env::temp_dir().join(format!(
                "winsched-runtime-state-{}-{}.json",
                std::process::id(),
                unix_time_ms()
            ));
            assert!(
                load_runtime_state(&path, ControllerMode::Auto)
                    .unwrap()
                    .scheduling_enabled
            );
            assert!(
                !load_runtime_state(&path, ControllerMode::Observe)
                    .unwrap()
                    .scheduling_enabled
            );

            let disabled = RuntimeState {
                schema_version: RUNTIME_SCHEMA_VERSION,
                scheduling_enabled: false,
            };
            persist_runtime_state(Some(&path), &disabled).unwrap();
            assert_eq!(
                load_runtime_state(&path, ControllerMode::Auto).unwrap(),
                disabled
            );
            fs::remove_file(path).unwrap();
        }

        #[test]
        fn observe_evaluates_without_enabling_mutations() {
            let observe = ControllerConfig {
                controller_mode: ControllerMode::Observe,
                ..ControllerConfig::default()
            };
            let disabled = RuntimeState {
                schema_version: RUNTIME_SCHEMA_VERSION,
                scheduling_enabled: false,
            };
            assert!(controller_evaluation_active(&observe, &disabled));
            assert!(!controller_mutations_active(&observe, &disabled));

            let auto = ControllerConfig {
                controller_mode: ControllerMode::Auto,
                ..ControllerConfig::default()
            };
            assert!(!controller_evaluation_active(&auto, &disabled));
            let enabled = RuntimeState {
                scheduling_enabled: true,
                ..disabled
            };
            assert!(controller_evaluation_active(&auto, &enabled));
            assert!(controller_mutations_active(&auto, &enabled));
        }

        #[test]
        fn control_channel_delivers_enable_disable_and_stop_without_polling() {
            let (sender, receiver) = mpsc::channel();
            sender.send(ControllerCommand::Enable).unwrap();
            sender.send(ControllerCommand::Disable).unwrap();
            sender.send(ControllerCommand::Stop).unwrap();
            assert_eq!(
                wait_for_command(Some(&receiver), Duration::from_secs(1)),
                ControllerCommand::Enable
            );
            assert_eq!(
                wait_for_command(Some(&receiver), Duration::from_secs(1)),
                ControllerCommand::Disable
            );
            assert_eq!(
                wait_for_command(Some(&receiver), Duration::from_secs(1)),
                ControllerCommand::Stop
            );
        }
    }
}
