#[cfg(any(windows, test))]
mod event_logger;

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
    use std::cmp::Reverse;
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::{OsStr, OsString};
    use std::fs::{self, File, OpenOptions};
    use std::io::Write;
    use std::num::NonZeroU16;
    use std::path::{Path, PathBuf};
    use std::process::Command as ProcessCommand;
    use std::sync::{OnceLock, mpsc};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use clap::{Parser, Subcommand};
    use serde::{Deserialize, Serialize};
    use serde_json::{Value, json};
    use thiserror::Error;
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
    use winsched::platform::{self, MutationReport};
    use winsched_config::{ControllerConfig, ControllerMode, LoggingConfig};
    use winsched_control::{
        CONFIG_FILE_NAME, CONTROL_DISABLE, CONTROL_ENABLE, ConfigReloadResult, ControllerPhase,
        ControllerStatus, INSTALL_DIRECTORY_NAME, LOG_FILE_NAME, MANAGED_STATE_FILE_NAME,
        RUNTIME_SCHEMA_VERSION, RUNTIME_STATE_FILE_NAME, RuntimeState, SERVICE_NAME,
        STATUS_FILE_NAME,
    };
    use winsched_core::adaptive::{
        AssignmentOrigin, DecisionReason, PolicyAction, PolicyDecision, PolicyEngine, ProcessKey,
    };

    const SERVICE_DISPLAY_NAME: &str = "WinSched LLC-aware placement controller";
    const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;
    const STATE_SCHEMA_VERSION: u32 = 1;
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

    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ManagedProcess {
        key: ProcessKey,
        domain: winsched_core::LlcDomainKey,
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
        },
        Start,
        Stop,
        Enable,
        Disable,
        Status,
        Uninstall,
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
        #[error("unsupported runtime-state schema {0}")]
        RuntimeStateSchema(u32),
        #[error("controller_mode=auto requires explicit --allow-auto during installation")]
        AutoNeedsConfirmation,
        #[error("service configuration path was not initialized")]
        MissingServiceConfig,
        #[error("failed to clear {0} managed CPU Set assignment(s)")]
        CleanupIncomplete(usize),
        #[error("service did not stop before the 20-second timeout")]
        ServiceStopTimeout,
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
                start,
                allow_auto,
            } => install(&config, start, allow_auto),
            Command::Register {
                config,
                start,
                allow_auto,
            } => register_in_place(&config, start, allow_auto),
            Command::Provision {
                config,
                start,
                allow_auto,
            } => provision_in_place(&config, start, allow_auto),
            Command::Start => start(),
            Command::Stop => stop(),
            Command::Enable => set_scheduling(true),
            Command::Disable => set_scheduling(false),
            Command::Status => status(),
            Command::Uninstall => uninstall(),
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

        let config = load_config(config_path)?;
        let install_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
        let log_path = install_dir.join(LOG_FILE_NAME);
        let managed_state_path = install_dir.join(MANAGED_STATE_FILE_NAME);
        let runtime_state_path = install_dir.join(RUNTIME_STATE_FILE_NAME);
        let status_path = install_dir.join(STATUS_FILE_NAME);
        let mut logger = EventLogger::service(log_path, config.logging)?;
        status_handle.set_service_status(service_status(ServiceState::Running, 0))?;

        let result = run_controller(
            config,
            ControllerFiles {
                config: Some(config_path),
                managed_state: Some(&managed_state_path),
                runtime_state: Some(&runtime_state_path),
                status: Some(&status_path),
            },
            Some(&control_rx),
            None,
            &mut logger,
        );
        let exit_code = u32::from(result.is_err());
        status_handle.set_service_status(service_status(ServiceState::Stopped, exit_code))?;
        result
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

    #[allow(clippy::too_many_lines)]
    fn run_controller(
        mut config: ControllerConfig,
        files: ControllerFiles<'_>,
        control: Option<&mpsc::Receiver<ControllerCommand>>,
        max_iterations: Option<u16>,
        logger: &mut EventLogger,
    ) -> Result<(), ServiceError> {
        let topology = platform::system_topology()?;
        let mut sampler = platform::LoadSampler::new(&topology)?;
        let mut engine = PolicyEngine::new(config.policy)?;
        let mut managed = files
            .managed_state
            .map_or_else(|| Ok(BTreeMap::new()), load_managed_state)?;
        let mut runtime = files.runtime_state.map_or_else(
            || Ok(RuntimeState::for_controller_mode(config.controller_mode)),
            |path| load_runtime_state(path, config.controller_mode),
        )?;
        let mut previous_cpu_times = BTreeMap::<ProcessKey, u64>::new();
        let started = Instant::now();
        let mut iteration = 0u64;
        // Re-read once on the first tick. This closes the narrow startup race where Settings
        // can replace the file after the service loaded it but before its first metadata read.
        let mut config_modified = files.config.map(|_| SystemTime::UNIX_EPOCH);
        let mut status = ControllerStatus::starting(
            std::process::id(),
            runtime.scheduling_enabled,
            config.controller_mode,
            config.fingerprint(),
            config.logging,
            topology.llc_domains.len(),
            unix_time_ms(),
        );
        status.phase = if runtime.scheduling_enabled {
            ControllerPhase::Running
        } else {
            ControllerPhase::Disabled
        };
        if !runtime.scheduling_enabled {
            let cleanup = cleanup_managed(logger, &mut managed, files.managed_state)?;
            status.last_error = cleanup_error(cleanup);
        }
        status.managed_processes = managed.len();
        persist_controller_status(files.status, &mut status)?;
        sampler.prime()?;
        logger.emit(json!({
            "event": "controller_started",
            "controller_mode": config.controller_mode,
            "scheduling_enabled": runtime.scheduling_enabled,
            "llc_domains": topology.llc_domains.len(),
            "rules": config.rules.len(),
        }));

        let loop_result: Result<(), ServiceError> = loop {
            let interval = Duration::from_millis(config.sample_interval_ms);
            match wait_for_command(control, interval) {
                ControllerCommand::Stop => {
                    status.phase = ControllerPhase::Stopping;
                    persist_controller_status(files.status, &mut status)?;
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
                    status.last_activity = Some("Scheduling enabled from tray or CLI".to_owned());
                    status.last_error = None;
                    persist_controller_status(files.status, &mut status)?;
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
                    status.scheduling_enabled = false;
                    status.phase = ControllerPhase::Disabled;
                    status.managed_processes = managed.len();
                    status.last_activity = Some(if cleanup.failed == 0 {
                        "Scheduling disabled; managed assignments cleared".to_owned()
                    } else {
                        format!(
                            "Scheduling disabled; {} assignment(s) await cleanup retry",
                            cleanup.failed
                        )
                    });
                    status.last_error = cleanup_error(cleanup);
                    persist_controller_status(files.status, &mut status)?;
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
            if let Some(event) = apply_reload_status(&mut status, &config, reload) {
                // This status receipt is authoritative for Settings and must reach disk before
                // any optional event-log write or rotation can fail.
                persist_controller_status(files.status, &mut status)?;
                logger.emit(event);
            }
            if !runtime.scheduling_enabled {
                let cleanup = cleanup_managed(logger, &mut managed, files.managed_state)?;
                status.phase = ControllerPhase::Disabled;
                status.managed_processes = managed.len();
                status.last_error = cleanup_error(cleanup);
                persist_controller_status(files.status, &mut status)?;
                continue;
            }
            let loads = match sampler.sample() {
                Ok(loads) => loads,
                Err(error) => break Err(error.into()),
            };
            let processes = match platform::observe_processes(&topology) {
                Ok(processes) => processes,
                Err(error) => break Err(error.into()),
            };
            let live = processes
                .iter()
                .map(|process| process.key)
                .collect::<BTreeSet<_>>();
            reconcile_managed(logger, &mut managed, files.managed_state, &processes)?;
            previous_cpu_times.retain(|key, _| live.contains(key));

            let observations = build_ranked_observations(
                &config,
                &managed,
                &processes,
                &mut previous_cpu_times,
                config.sample_interval_ms,
            );

            let evaluation_time_ms =
                u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let decisions =
                match engine.evaluate(evaluation_time_ms, &topology, &loads, &observations) {
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

            iteration = iteration.saturating_add(1);
            status.iteration = iteration;
            status.managed_processes = managed.len();
            status.phase = ControllerPhase::Running;
            persist_controller_status(files.status, &mut status)?;
            if max_iterations.is_some_and(|limit| iteration >= u64::from(limit)) {
                break Ok(());
            }
        };

        let cleanup_result =
            cleanup_managed(logger, &mut managed, files.managed_state).and_then(|report| {
                if report.failed == 0 {
                    Ok(())
                } else {
                    Err(ServiceError::CleanupIncomplete(report.failed))
                }
            });
        status.phase = if loop_result.is_ok() && cleanup_result.is_ok() {
            ControllerPhase::Stopped
        } else {
            ControllerPhase::Error
        };
        status.managed_processes = managed.len();
        if let Err(error) = &loop_result {
            status.last_error = Some(error.to_string());
        } else if let Err(error) = &cleanup_result {
            status.last_error = Some(error.to_string());
        }
        let status_result = persist_controller_status(files.status, &mut status);
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

    fn build_ranked_observations(
        config: &ControllerConfig,
        managed: &BTreeMap<ProcessKey, winsched_core::LlcDomainKey>,
        processes: &[platform::ObservedProcess],
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
            let recovered = managed.contains_key(&process.key);
            let explicit_rule = config
                .rules
                .iter()
                .any(|rule| rule.image.eq_ignore_ascii_case(&process.image_name));
            let rule = config.resolve(&process.image_name);
            let (placement, enforcement) = match (recovered, config.controller_mode, rule) {
                (true, mode, _) if mode != ControllerMode::Auto => (
                    winsched_core::adaptive::PlacementMode::Off,
                    winsched_core::adaptive::EnforcementMode::Apply,
                ),
                (_, _, Some(rule)) => (rule.placement, rule.enforcement),
                (true, _, None) => (
                    winsched_core::adaptive::PlacementMode::Off,
                    winsched_core::adaptive::EnforcementMode::Apply,
                ),
                (false, _, None) => continue,
            };
            if !recovered
                && (!explicit_rule
                    && process_utilization_bps(cpu_delta, sample_interval_ms)
                        < u32::from(config.minimum_process_utilization_bps)
                    || process.exclusion.is_some())
            {
                continue;
            }
            let mut observation = process.policy_observation(placement, enforcement);
            if recovered {
                observation.assignment_origin = AssignmentOrigin::Managed;
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
        managed: &mut BTreeMap<ProcessKey, winsched_core::LlcDomainKey>,
        state_path: Option<&Path>,
        processes: &[platform::ObservedProcess],
    ) -> Result<(), ServiceError> {
        let entries = managed
            .iter()
            .map(|(key, domain)| (*key, *domain))
            .collect::<Vec<_>>();
        let mut changed = false;
        for (key, domain) in entries {
            let Some(process) = processes.iter().find(|process| process.key == key) else {
                managed.remove(&key);
                changed = true;
                continue;
            };
            if process.current_domain != Some(domain) {
                managed.remove(&key);
                changed = true;
                continue;
            }
            let Some(exclusion) = process.exclusion else {
                continue;
            };
            let result = platform::clear_process_key(key);
            logger.emit(json!({
                "event": "cleanup_excluded",
                "process": key,
                "exclusion": exclusion,
                "succeeded": result.is_ok(),
                "error": result.as_ref().err().map(ToString::to_string),
            }));
            if result.is_ok() {
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
        reload: ConfigReload,
    ) -> Option<Value> {
        status.configured_mode = config.controller_mode;
        status.applied_config_fingerprint = config.fingerprint();
        status.applied_logging = config.logging;
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
        managed: &mut BTreeMap<ProcessKey, winsched_core::LlcDomainKey>,
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
            PolicyAction::Assign { target, .. } => format!(
                "assigned to LLC {}:{}",
                target.group, target.last_level_cache_index
            ),
            PolicyAction::Move { source, target, .. } => format!(
                "moved LLC {}:{} -> {}:{}",
                source.group,
                source.last_level_cache_index,
                target.group,
                target.last_level_cache_index
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
        managed: &mut BTreeMap<ProcessKey, winsched_core::LlcDomainKey>,
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
        managed: &mut BTreeMap<ProcessKey, winsched_core::LlcDomainKey>,
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
                managed.insert(decision.process, target);
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
        managed: &mut BTreeMap<ProcessKey, winsched_core::LlcDomainKey>,
        state_path: Option<&Path>,
    ) -> Result<CleanupReport, ServiceError> {
        cleanup_managed_with(logger, managed, state_path, |process| {
            platform::clear_process_key(process)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }

    fn cleanup_managed_with<F>(
        logger: &mut EventLogger,
        managed: &mut BTreeMap<ProcessKey, winsched_core::LlcDomainKey>,
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

    fn cleanup_error(report: CleanupReport) -> Option<String> {
        (report.failed != 0).then(|| {
            format!(
                "failed to clear {} managed CPU Set assignment(s); retry pending",
                report.failed
            )
        })
    }

    fn load_managed_state(
        path: &Path,
    ) -> Result<BTreeMap<ProcessKey, winsched_core::LlcDomainKey>, ServiceError> {
        if !path.exists() {
            return Ok(BTreeMap::new());
        }
        let state = serde_json::from_slice::<ManagedStateFile>(&fs::read(path)?)?;
        if state.schema_version != STATE_SCHEMA_VERSION {
            return Err(ServiceError::StateSchema(state.schema_version));
        }
        Ok(state
            .processes
            .into_iter()
            .map(|process| (process.key, process.domain))
            .collect())
    }

    fn persist_managed_state(
        path: Option<&Path>,
        managed: &BTreeMap<ProcessKey, winsched_core::LlcDomainKey>,
    ) -> Result<(), ServiceError> {
        let Some(path) = path else {
            return Ok(());
        };
        let state = ManagedStateFile {
            schema_version: STATE_SCHEMA_VERSION,
            processes: managed
                .iter()
                .map(|(key, domain)| ManagedProcess {
                    key: *key,
                    domain: *domain,
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
        if !path.exists() {
            return Ok(RuntimeState::for_controller_mode(configured_mode));
        }
        let state = serde_json::from_slice::<RuntimeState>(&fs::read(path)?)?;
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

    fn install(config_path: &Path, start_now: bool, allow_auto: bool) -> Result<(), ServiceError> {
        let config = validated_registration_config(config_path, allow_auto)?;
        let install_dir = program_data_dir().join(INSTALL_DIRECTORY_NAME);
        fs::create_dir_all(&install_dir)?;
        let installed_exe = install_dir.join("winsched-service.exe");
        let installed_config = install_dir.join(CONFIG_FILE_NAME);
        let current_exe = std::env::current_exe()?;
        if current_exe != installed_exe {
            fs::copy(&current_exe, &installed_exe)?;
        }
        atomic_write(
            &installed_config,
            toml::to_string_pretty(&config)?.as_bytes(),
        )?;
        configure_service(&installed_exe, &installed_config, start_now, false)?;
        println!("installed {SERVICE_NAME}");
        Ok(())
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
        configure_service(&current_exe, &absolute_config, start_now, false)?;
        println!("registered {SERVICE_NAME}");
        Ok(())
    }

    fn provision_in_place(
        config_path: &Path,
        start_now: bool,
        allow_auto: bool,
    ) -> Result<(), ServiceError> {
        let config = validated_registration_config(config_path, allow_auto)?;
        let _ = config;
        let current_exe = std::env::current_exe()?;
        let absolute_config = fs::canonicalize(config_path)?;
        configure_service(&current_exe, &absolute_config, start_now, true)?;
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

        let configure_result = apply_service_settings(&service, start_now);
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
        let output = ProcessCommand::new(system_sc_path()).args(args).output()?;
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

    fn system_sc_path() -> PathBuf {
        std::env::var_os("SystemRoot")
            .or_else(|| std::env::var_os("WINDIR"))
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
            .join("System32")
            .join("sc.exe")
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
        let output = ProcessCommand::new("sc.exe")
            .args(["sdset", SERVICE_NAME, INTERACTIVE_SERVICE_SDDL])
            .output()?;
        if output.status.success() {
            return Ok(());
        }
        Err(ServiceError::CommandFailed {
            program: "sc.exe sdset",
            exit_code: output.status.code(),
            stderr: command_failure_text(&output),
        })
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

    fn uninstall() -> Result<(), ServiceError> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        let Some(service) = open_service_optional(
            &manager,
            SERVICE_NAME,
            ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
        )?
        else {
            println!("service is not installed");
            return Ok(());
        };
        ensure_service_stopped(&service)?;
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
        let temporary = path.with_extension("tmp");
        let backup = path.with_extension("bak");
        let mut file = File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);

        if backup.exists() {
            fs::remove_file(&backup)?;
        }
        let had_original = path.exists();
        if had_original {
            fs::rename(path, &backup)?;
        }
        if let Err(error) = fs::rename(&temporary, path) {
            if had_original {
                let _ = fs::rename(&backup, path);
            }
            return Err(error);
        }
        if had_original {
            fs::remove_file(backup)?;
        }
        Ok(())
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
                ControllerMode::Observe,
                ControllerConfig::default().fingerprint(),
                LoggingConfig::default(),
                2,
                unix_time_ms(),
            );

            let event = apply_reload_status(&mut status, &config, ConfigReload::Reloaded).unwrap();
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
                config.controller_mode,
                config.fingerprint(),
                config.logging,
                2,
                unix_time_ms(),
            );
            let event = apply_reload_status(&mut status, &config, reload).unwrap();
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
                winsched_core::LlcDomainKey {
                    group: 0,
                    last_level_cache_index: 1,
                },
            )]);
            let mut previous = BTreeMap::new();
            let ranked =
                build_ranked_observations(&config, &managed, &[observed], &mut previous, 1_000);
            assert_eq!(ranked.len(), 1);
            assert_eq!(ranked[0].mode, winsched_core::adaptive::PlacementMode::Off);
            assert_eq!(
                ranked[0].enforcement,
                winsched_core::adaptive::EnforcementMode::Apply
            );
            assert_eq!(ranked[0].assignment_origin, AssignmentOrigin::Managed);
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
            let managed = BTreeMap::from([(key, domain)]);

            persist_managed_state(Some(&path), &managed).unwrap();
            assert_eq!(load_managed_state(&path).unwrap(), managed);
            persist_managed_state(Some(&path), &BTreeMap::new()).unwrap();
            assert!(load_managed_state(&path).unwrap().is_empty());

            fs::remove_file(&path).unwrap();
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
            let mut managed = BTreeMap::from([(first, domain), (second, domain)]);
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
            assert_eq!(managed, BTreeMap::from([(second, domain)]));
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
            let mut managed = BTreeMap::from([(key, domain)]);
            let mut logger = EventLogger::console();

            let result = cleanup_managed_with(&mut logger, &mut managed, Some(&path), |_| {
                Err("injected clear failure".to_owned())
            });
            assert!(matches!(result, Err(ServiceError::Io(_))));
            assert_eq!(managed, BTreeMap::from([(key, domain)]));
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
