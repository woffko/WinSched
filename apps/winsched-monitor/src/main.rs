#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("winsched-monitor is only available on Windows");
}

#[cfg(windows)]
fn main() {
    if let Err(error) = app::run() {
        app::show_startup_error(&error.to_string());
    }
}

#[cfg(windows)]
mod app {
    #![allow(unsafe_code)] // Narrow Win32 activation and ShellExecute calls are documented.

    use std::collections::{BTreeMap, BTreeSet};
    use std::error::Error;
    use std::ffi::{OsStr, OsString};
    use std::fs::{self, File, OpenOptions};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use eframe::egui::{self, Color32, RichText};
    use serde::{Deserialize, Serialize};
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    use windows::Win32::System::Threading::{
        ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, CreateEventW, EVENT_MODIFY_STATE,
        HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, INFINITE, NORMAL_PRIORITY_CLASS, OpenEventW,
        REALTIME_PRIORITY_CLASS, SetEvent, WaitForSingleObject,
    };
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_OK, MessageBoxW, SW_SHOWNORMAL,
    };
    use windows::core::{PCWSTR, w};
    use windows_service::Error as WindowsServiceError;
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use winsched::platform::{
        MonitoredProcess, ProcessEcoQosState, ProcessMemoryPriority, monitor_processes,
        system_topology,
    };
    use winsched_config::{ControllerConfig, ProcessRule};
    use winsched_control::{
        CONFIG_FILE_NAME, ControllerStatus, INSTALL_DIRECTORY_NAME, MANAGED_STATE_FILE_NAME,
        SERVICE_NAME, STATUS_FILE_NAME, STATUS_SCHEMA_VERSION,
    };
    use winsched_core::Topology;
    use winsched_core::adaptive::{ExclusionReason, ProcessKey};

    const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
    const ACTIVE_REPAINT_INTERVAL: Duration = Duration::from_millis(100);
    const STATUS_STALE_AFTER_MS: u64 = 75_000;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Language {
        English,
        Russian,
    }

    impl Language {
        const fn text<'a>(self, english: &'a str, russian: &'a str) -> &'a str {
            match self {
                Self::English => english,
                Self::Russian => russian,
            }
        }
    }

    #[derive(Debug, Clone)]
    struct Paths {
        config: PathBuf,
        status: PathBuf,
        managed_state: PathBuf,
        settings: PathBuf,
        instance_lock: PathBuf,
    }

    impl Paths {
        fn discover() -> Self {
            let program_data = std::env::var_os("PROGRAMDATA")
                .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from);
            let install_dir = program_data.join(INSTALL_DIRECTORY_NAME);
            let binary_dir = std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(Path::to_path_buf))
                .unwrap_or_else(|| install_dir.clone());
            let local_data = std::env::var_os("LOCALAPPDATA")
                .map_or_else(std::env::temp_dir, PathBuf::from)
                .join("WinSched");
            Self {
                config: install_dir.join(CONFIG_FILE_NAME),
                status: install_dir.join(STATUS_FILE_NAME),
                managed_state: install_dir.join(MANAGED_STATE_FILE_NAME),
                settings: binary_dir.join("winsched-settings.exe"),
                instance_lock: local_data.join("winsched-monitor.lock"),
            }
        }
    }

    struct InstanceLock {
        _file: File,
    }

    impl InstanceLock {
        fn try_acquire(path: &Path) -> Result<Option<Self>, std::io::Error> {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .share_mode(0)
                .open(path)
            {
                Ok(file) => Ok(Some(Self { _file: file })),
                Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => Ok(None),
                Err(error) => Err(error),
            }
        }
    }

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: This wrapper owns the event handle exactly once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ServiceViewState {
        Missing,
        Stopped,
        Running,
        StartPending,
        StopPending,
        Other,
        Error,
    }

    #[derive(Debug)]
    struct Snapshot {
        captured: Instant,
        service: ServiceViewState,
        status: Option<ControllerStatus>,
        config: Option<ControllerConfig>,
        managed: BTreeSet<ProcessKey>,
        topology: Arc<Topology>,
        processes: Vec<MonitoredProcess>,
    }

    #[derive(Debug)]
    struct SnapshotError(String);

    struct PendingSnapshot {
        receiver: mpsc::Receiver<Result<Snapshot, SnapshotError>>,
    }

    #[derive(Debug, Deserialize)]
    struct ManagedStateDocument {
        schema_version: u32,
        #[serde(default)]
        processes: Vec<ManagedStateProcess>,
    }

    #[derive(Debug, Deserialize)]
    struct ManagedStateProcess {
        key: ProcessKey,
    }

    #[derive(Debug, Serialize)]
    struct SnapshotSelfTestReceipt {
        result: &'static str,
        processes: usize,
        working_set_available: usize,
        efficiency_available: usize,
        cpu_set_assignments: usize,
        service: String,
        status_schema: Option<u32>,
    }

    #[derive(Debug, Default)]
    struct MonitorLaunchOptions {
        snapshot_self_test: Option<PathBuf>,
        test_observation_file: Option<PathBuf>,
    }

    #[derive(Debug, Serialize)]
    struct TestObservationReceipt {
        schema_version: u32,
        snapshots_started: u64,
    }

    #[derive(Debug, Clone)]
    struct ProcessRow {
        key: ProcessKey,
        image: String,
        session_id: Option<u32>,
        cpu_basis_points: Option<u32>,
        working_set_bytes: Option<u64>,
        thread_count: u32,
        priority_class: Option<u32>,
        cpu_sets: Vec<u32>,
        llc: String,
        eco_qos: Option<ProcessEcoQosState>,
        memory_priority: Option<ProcessMemoryPriority>,
        scope: String,
        has_exact_rule: bool,
        assignment: String,
        exclusion: Option<ExclusionReason>,
    }

    struct ProcessMonitorApp {
        paths: Paths,
        language: Language,
        rows: Vec<ProcessRow>,
        service: ServiceViewState,
        status: Option<ControllerStatus>,
        filter: String,
        show_all_sessions: bool,
        pending: Option<PendingSnapshot>,
        next_refresh: Instant,
        last_capture: Option<Instant>,
        prior_cpu: BTreeMap<ProcessKey, u64>,
        last_error: Option<String>,
        last_active: bool,
        snapshot_count: u64,
        snapshots_started: u64,
        test_observation_file: Option<PathBuf>,
        topology: Option<Arc<Topology>>,
    }

    impl ProcessMonitorApp {
        fn new(paths: Paths, language: Language, test_observation_file: Option<PathBuf>) -> Self {
            Self {
                paths,
                language,
                rows: Vec::new(),
                service: ServiceViewState::Error,
                status: None,
                filter: String::new(),
                show_all_sessions: false,
                pending: None,
                next_refresh: Instant::now(),
                last_capture: None,
                prior_cpu: BTreeMap::new(),
                last_error: None,
                last_active: true,
                snapshot_count: 0,
                snapshots_started: 0,
                test_observation_file,
                topology: None,
            }
        }

        fn start_refresh(&mut self) {
            if self.pending.is_some() {
                return;
            }
            let paths = self.paths.clone();
            let show_all_sessions = self.show_all_sessions;
            let topology = self.topology.clone();
            let (sender, receiver) = mpsc::channel();
            self.pending = Some(PendingSnapshot { receiver });
            self.snapshots_started = self.snapshots_started.saturating_add(1);
            if let Some(path) = &self.test_observation_file
                && let Err(error) = write_test_observation(path, self.snapshots_started)
            {
                self.last_error = Some(format!("cannot write test observation: {error}"));
            }
            std::thread::Builder::new()
                .name("winsched-process-monitor-snapshot".to_owned())
                .spawn(move || {
                    let result = collect_snapshot(&paths, show_all_sessions, topology);
                    let _ = sender.send(result);
                })
                .map_or_else(
                    |error| {
                        self.pending = None;
                        self.last_error = Some(format!("cannot start snapshot worker: {error}"));
                    },
                    |_| {},
                );
        }

        fn poll_refresh(&mut self) {
            let Some(pending) = &self.pending else {
                return;
            };
            match pending.receiver.try_recv() {
                Ok(Ok(snapshot)) => {
                    self.pending = None;
                    self.apply_snapshot(snapshot);
                }
                Ok(Err(error)) => {
                    self.pending = None;
                    self.last_error = Some(error.0);
                    self.next_refresh = Instant::now() + REFRESH_INTERVAL;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.last_error = Some("snapshot worker exited without a result".to_owned());
                    self.next_refresh = Instant::now() + REFRESH_INTERVAL;
                }
            }
        }

        fn apply_snapshot(&mut self, snapshot: Snapshot) {
            let elapsed = self
                .last_capture
                .map(|previous| snapshot.captured.saturating_duration_since(previous));
            let mut next_cpu = BTreeMap::new();
            self.rows = snapshot
                .processes
                .into_iter()
                .map(|process| {
                    let observed = process.process;
                    let cpu_basis_points = elapsed.and_then(|elapsed| {
                        let previous = self.prior_cpu.get(&observed.key)?;
                        cpu_basis_points(*previous, observed.cpu_time_100ns, elapsed)
                    });
                    next_cpu.insert(observed.key, observed.cpu_time_100ns);
                    let exact_rule = snapshot.config.as_ref().and_then(|config| {
                        config
                            .rules
                            .iter()
                            .find(|rule| rule.image.eq_ignore_ascii_case(&observed.image_name))
                    });
                    let scope = process_scope_label(
                        snapshot.config.as_ref(),
                        exact_rule,
                        &observed.image_name,
                        observed.exclusion,
                    );
                    let assignment = if snapshot.managed.contains(&observed.key) {
                        "WinSched".to_owned()
                    } else if observed.default_cpu_set_ids.is_empty() {
                        "None".to_owned()
                    } else {
                        "External".to_owned()
                    };
                    ProcessRow {
                        key: observed.key,
                        image: observed.image_name,
                        session_id: observed.session_id,
                        cpu_basis_points,
                        working_set_bytes: process.working_set_bytes,
                        thread_count: observed.thread_count,
                        priority_class: observed.priority_class,
                        cpu_sets: observed.default_cpu_set_ids,
                        llc: observed.current_domain.map_or_else(
                            || "—".to_owned(),
                            |domain| format!("{}:{}", domain.group, domain.last_level_cache_index),
                        ),
                        eco_qos: process.efficiency.map(|state| state.eco_qos),
                        memory_priority: process.efficiency.map(|state| state.memory_priority),
                        scope,
                        has_exact_rule: exact_rule.is_some(),
                        assignment,
                        exclusion: observed.exclusion,
                    }
                })
                .collect();
            self.rows.sort_by(|left, right| {
                right
                    .cpu_basis_points
                    .unwrap_or_default()
                    .cmp(&left.cpu_basis_points.unwrap_or_default())
                    .then_with(|| left.image.cmp(&right.image))
                    .then_with(|| left.key.pid.cmp(&right.key.pid))
            });
            self.prior_cpu = next_cpu;
            self.last_capture = Some(snapshot.captured);
            self.service = snapshot.service;
            self.status = snapshot.status;
            self.topology = Some(snapshot.topology);
            self.last_error = None;
            self.snapshot_count = self.snapshot_count.saturating_add(1);
            self.next_refresh = Instant::now() + REFRESH_INTERVAL;
        }

        fn status_header(&mut self, ui: &mut egui::Ui) {
            let language = self.language;
            ui.horizontal_wrapped(|ui| {
                ui.heading(language.text("WinSched Processes", "Процессы WinSched"));
                ui.separator();
                ui.label(format!(
                    "{}: {}",
                    language.text("Service", "Служба"),
                    service_label(self.service, language)
                ));
                if let Some(status) = &self.status {
                    ui.label(format!(
                        "{}: {}",
                        language.text("Scheduling", "Планирование"),
                        if status.scheduling_enabled {
                            language.text("Enabled", "Включено")
                        } else {
                            language.text("Disabled", "Выключено")
                        }
                    ));
                    ui.label(format!(
                        "{}: {}",
                        language.text("Managed", "Управляются"),
                        status.managed_processes
                    ));
                    ui.label(format!(
                        "{}: {:?}",
                        language.text("Mode", "Режим"),
                        status.configured_mode
                    ));
                }
                ui.label(format!(
                    "{}: {}",
                    language.text("Snapshots", "Снимки"),
                    self.snapshot_count
                ));
                if ui
                    .button(language.text("Settings…", "Настройки…"))
                    .clicked()
                    && let Err(error) = launch_settings(&self.paths.settings, None)
                {
                    self.last_error = Some(error);
                }
                if ui.button(language.text("Refresh", "Обновить")).clicked() {
                    self.next_refresh = Instant::now();
                }
            });
            if let Some(status) = &self.status
                && unix_time_ms().saturating_sub(status.updated_at_unix_ms) > STATUS_STALE_AFTER_MS
            {
                ui.colored_label(
                    Color32::YELLOW,
                    language.text(
                        "Service status heartbeat is stale.",
                        "Данные службы устарели.",
                    ),
                );
            }
            if let Some(status) = &self.status {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!(
                        "{}: {}/{} {}",
                        language.text("System reserve", "Системный резерв"),
                        status.system_reserve.reserved_physical_cores.len(),
                        status.system_reserve.physical_core_count,
                        language.text("cores", "ядер")
                    ));
                    ui.label(format!(
                        "Scheduler p99: {} µs",
                        status.scheduler_latency.p99_lateness_us
                    ));
                    ui.label(format!(
                        "Background: {} {} / {} {}",
                        status.background_efficiency.managed_processes,
                        language.text("managed", "управляются"),
                        status.background_efficiency.protected_processes,
                        language.text("protected", "защищены")
                    ));
                    ui.label(format!(
                        "{}: {}",
                        language.text("Last activity", "Последняя активность"),
                        status.last_activity.as_deref().unwrap_or("—")
                    ));
                    if let Some(error) = &status.last_error {
                        ui.colored_label(
                            Color32::from_rgb(220, 80, 80),
                            format!("{}: {error}", language.text("Error", "Ошибка")),
                        );
                    }
                });
            }
            if let Some(error) = &self.last_error {
                ui.colored_label(Color32::from_rgb(220, 80, 80), error);
            }
        }

        fn process_table(&mut self, ui: &mut egui::Ui) {
            let language = self.language;
            let filter = self.filter.trim().to_ascii_lowercase();
            let mut rule_request = None::<String>;
            egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                egui::Grid::new("process-monitor-grid")
                    .striped(true)
                    .min_col_width(58.0)
                    .show(ui, |ui| {
                        for title in [
                            language.text("Process", "Процесс"),
                            "PID",
                            language.text("Session", "Сеанс"),
                            "CPU %",
                            language.text("RAM", "ОЗУ"),
                            language.text("Threads", "Потоки"),
                            language.text("Priority", "Приоритет"),
                            "CPU Sets",
                            "LLC",
                            "EcoQoS",
                            language.text("Memory", "Память"),
                            language.text("Rule / scope", "Правило / область"),
                            language.text("Assignment", "Назначение"),
                        ] {
                            ui.label(RichText::new(title).strong());
                        }
                        ui.end_row();

                        for row in self.rows.iter().filter(|row| {
                            filter.is_empty()
                                || row.image.to_ascii_lowercase().contains(&filter)
                                || row.key.pid.to_string().contains(&filter)
                        }) {
                            let response = ui.selectable_label(false, &row.image);
                            response.context_menu(|ui| {
                                let label = if row.scope.starts_with("Exact") {
                                    language.text("Edit exact rule…", "Изменить точное правило…")
                                } else {
                                    language.text("Create exact rule…", "Создать точное правило…")
                                };
                                let allowed = row.has_exact_rule || row.exclusion.is_none();
                                if ui.add_enabled(allowed, egui::Button::new(label)).clicked() {
                                    rule_request = Some(row.image.clone());
                                    ui.close();
                                }
                                if !allowed {
                                    ui.label(language.text(
                                        "Safety-excluded processes cannot be opted into automatic control.",
                                        "Процессы из защитных исключений нельзя включить в автоматическое управление.",
                                    ));
                                }
                                ui.label(language.text(
                                    "The draft applies to every process with this executable name and is not saved until Apply.",
                                    "Черновик применяется ко всем процессам с этим именем и не сохраняется до нажатия «Применить».",
                                ));
                            });
                            ui.label(row.key.pid.to_string());
                            ui.label(row.session_id.map_or_else(
                                || "—".to_owned(),
                                |session| session.to_string(),
                            ));
                            ui.label(row.cpu_basis_points.map_or_else(
                                || "—".to_owned(),
                                |value| format!("{}.{:02}", value / 100, value % 100),
                            ));
                            ui.label(row.working_set_bytes.map_or_else(
                                || "—".to_owned(),
                                format_mib,
                            ));
                            ui.label(row.thread_count.to_string());
                            ui.label(priority_label(row.priority_class));
                            ui.label(if row.cpu_sets.is_empty() {
                                "—".to_owned()
                            } else {
                                row.cpu_sets
                                    .iter()
                                    .map(u32::to_string)
                                    .collect::<Vec<_>>()
                                    .join(",")
                            });
                            ui.label(&row.llc);
                            ui.label(eco_qos_label(row.eco_qos));
                            ui.label(memory_priority_label(row.memory_priority));
                            ui.label(&row.scope);
                            ui.label(&row.assignment);
                            ui.end_row();
                        }
                    });
            });
            if let Some(image) = rule_request
                && let Err(error) = launch_settings(&self.paths.settings, Some(&image))
            {
                self.last_error = Some(error);
            }
        }
    }

    impl eframe::App for ProcessMonitorApp {
        fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
            let active = context.input(|input| {
                let viewport = input.viewport();
                sampling_active(viewport.focused, viewport.minimized, viewport.occluded)
            });
            if active {
                self.poll_refresh();
                if self.pending.is_none() && Instant::now() >= self.next_refresh {
                    self.start_refresh();
                }
                context.request_repaint_after(ACTIVE_REPAINT_INTERVAL);
            } else if self.last_active {
                self.next_refresh = Instant::now();
            }
            self.last_active = active;
        }

        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let active = self.last_active;
            egui::Panel::top("status").show(ui, |ui| {
                self.status_header(ui);
                ui.horizontal(|ui| {
                    ui.label(self.language.text("Filter", "Фильтр"));
                    ui.text_edit_singleline(&mut self.filter);
                    ui.checkbox(
                        &mut self.show_all_sessions,
                        self.language.text(
                            "Show all sessions and system processes",
                            "Показывать все сеансы и системные процессы",
                        ),
                    );
                    if !active {
                        ui.label(self.language.text(
                            "Sampling paused while the window is inactive.",
                            "Опрос приостановлен, пока окно неактивно.",
                        ));
                    } else if self.pending.is_some() {
                        ui.spinner();
                    }
                });
            });
            egui::CentralPanel::default().show(ui, |ui| self.process_table(ui));
        }
    }

    pub(super) fn run() -> Result<(), Box<dyn Error>> {
        let paths = Paths::discover();
        let options = monitor_launch_options()?;
        if let Some(output) = options.snapshot_self_test {
            return run_snapshot_self_test(&paths, &output);
        }
        let Some(instance) = InstanceLock::try_acquire(&paths.instance_lock)? else {
            signal_existing_instance()?;
            return Ok(());
        };
        let language = detect_language();
        let title = language.text("WinSched Process Monitor", "Монитор процессов WinSched");
        let native_options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size([1420.0, 780.0])
                .with_min_inner_size([980.0, 560.0]),
            ..Default::default()
        };
        eframe::run_native(
            title,
            native_options,
            Box::new(move |context| {
                context.egui_ctx.set_zoom_factor(1.0);
                start_activation_listener(context.egui_ctx.clone()).map_err(|error| {
                    Box::<dyn Error + Send + Sync>::from(std::io::Error::other(error))
                })?;
                Ok(Box::new(ProcessMonitorApp::new(
                    paths,
                    language,
                    options.test_observation_file,
                )))
            }),
        )?;
        drop(instance);
        Ok(())
    }

    fn monitor_launch_options() -> Result<MonitorLaunchOptions, std::io::Error> {
        let mut arguments = std::env::args_os().skip(1);
        let Some(argument) = arguments.next() else {
            return Ok(MonitorLaunchOptions::default());
        };
        if argument != "--snapshot-self-test" && argument != "--test-observation-file" {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unsupported Process Monitor command-line argument",
            ));
        }
        let output = arguments.next().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Process Monitor test option requires an output path",
            )
        })?;
        if arguments.next().is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unexpected arguments after Process Monitor test option",
            ));
        }
        let output = PathBuf::from(output);
        Ok(if argument == "--snapshot-self-test" {
            MonitorLaunchOptions {
                snapshot_self_test: Some(output),
                test_observation_file: None,
            }
        } else {
            MonitorLaunchOptions {
                snapshot_self_test: None,
                test_observation_file: Some(output),
            }
        })
    }

    fn write_test_observation(path: &Path, snapshots_started: u64) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let receipt = TestObservationReceipt {
            schema_version: 1,
            snapshots_started,
        };
        let encoded = serde_json::to_vec(&receipt).map_err(std::io::Error::other)?;
        winsched::platform::atomic_replace_file(path, &encoded)
    }

    fn run_snapshot_self_test(paths: &Paths, output: &Path) -> Result<(), Box<dyn Error>> {
        let snapshot =
            collect_snapshot(paths, false, None).map_err(|error| std::io::Error::other(error.0))?;
        let working_set_available = snapshot
            .processes
            .iter()
            .filter(|process| process.working_set_bytes.is_some())
            .count();
        let efficiency_available = snapshot
            .processes
            .iter()
            .filter(|process| process.efficiency.is_some())
            .count();
        let passed =
            !snapshot.processes.is_empty() && working_set_available > 0 && efficiency_available > 0;
        let receipt = SnapshotSelfTestReceipt {
            result: if passed { "PASS" } else { "FAIL" },
            processes: snapshot.processes.len(),
            working_set_available,
            efficiency_available,
            cpu_set_assignments: snapshot
                .processes
                .iter()
                .filter(|process| !process.process.default_cpu_set_ids.is_empty())
                .count(),
            service: format!("{:?}", snapshot.service),
            status_schema: snapshot.status.as_ref().map(|status| status.schema_version),
        };
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(output, serde_json::to_vec_pretty(&receipt)?)?;
        println!("{}", serde_json::to_string_pretty(&receipt)?);
        if !passed {
            return Err(std::io::Error::other(
                "Process Monitor snapshot self-test did not collect required process details",
            )
            .into());
        }
        Ok(())
    }

    fn collect_snapshot(
        paths: &Paths,
        show_all_sessions: bool,
        topology: Option<Arc<Topology>>,
    ) -> Result<Snapshot, SnapshotError> {
        let topology = topology
            .map_or_else(|| system_topology().map(Arc::new), Ok)
            .map_err(|error| SnapshotError(error.to_string()))?;
        let current_session = winsched::platform::current_session_id()
            .map_err(|error| SnapshotError(error.to_string()))?;
        let processes = monitor_processes(
            topology.as_ref(),
            (!show_all_sessions).then_some(current_session),
        )
        .map_err(|error| SnapshotError(error.to_string()))?;
        let config = fs::read_to_string(&paths.config)
            .ok()
            .and_then(|text| ControllerConfig::from_toml(&text).ok());
        let status = fs::read(&paths.status)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ControllerStatus>(&bytes).ok())
            .filter(|status| status.schema_version == STATUS_SCHEMA_VERSION);
        let managed = read_managed_keys(&paths.managed_state);
        Ok(Snapshot {
            captured: Instant::now(),
            service: query_service_state(),
            status,
            config,
            managed,
            topology,
            processes,
        })
    }

    fn read_managed_keys(path: &Path) -> BTreeSet<ProcessKey> {
        let Some(document) = fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ManagedStateDocument>(&bytes).ok())
        else {
            return BTreeSet::new();
        };
        if document.schema_version != 2 {
            return BTreeSet::new();
        }
        document
            .processes
            .into_iter()
            .map(|process| process.key)
            .collect()
    }

    fn cpu_basis_points(previous: u64, current: u64, elapsed: Duration) -> Option<u32> {
        let elapsed_ns = elapsed.as_nanos();
        if current < previous || elapsed_ns == 0 {
            return None;
        }
        let basis_points = u128::from(current - previous)
            .saturating_mul(1_000_000)
            .checked_div(elapsed_ns)?;
        Some(u32::try_from(basis_points).unwrap_or(u32::MAX))
    }

    const fn sampling_active(
        focused: Option<bool>,
        minimized: Option<bool>,
        occluded: Option<bool>,
    ) -> bool {
        matches!(focused, Some(true))
            && !matches!(minimized, Some(true))
            && !matches!(occluded, Some(true))
    }

    fn format_mib(bytes: u64) -> String {
        let tenths = bytes.saturating_mul(10) / 1_048_576;
        format!("{}.{:01} MiB", tenths / 10, tenths % 10)
    }

    fn process_scope_label(
        config: Option<&ControllerConfig>,
        exact: Option<&ProcessRule>,
        image: &str,
        exclusion: Option<ExclusionReason>,
    ) -> String {
        if let Some(exclusion) = exclusion {
            return format!("Excluded: {exclusion:?}");
        }
        if let Some(rule) = exact {
            return format!("Exact: {:?}/{:?}", rule.mode, rule.profile);
        }
        config.map_or_else(
            || "Config unavailable".to_owned(),
            |config| {
                config.resolve(image).map_or_else(
                    || "Out of scope".to_owned(),
                    |resolved| format!("Default: {:?}/{:?}", resolved.placement, resolved.profile),
                )
            },
        )
    }

    fn query_service_state() -> ServiceViewState {
        let result = (|| {
            let manager =
                ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
            let service = manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS)?;
            service.query_status()
        })();
        match result {
            Ok(status) => match status.current_state {
                ServiceState::Stopped => ServiceViewState::Stopped,
                ServiceState::Running => ServiceViewState::Running,
                ServiceState::StartPending => ServiceViewState::StartPending,
                ServiceState::StopPending => ServiceViewState::StopPending,
                _ => ServiceViewState::Other,
            },
            Err(WindowsServiceError::Winapi(error)) if error.raw_os_error() == Some(1060) => {
                ServiceViewState::Missing
            }
            Err(_) => ServiceViewState::Error,
        }
    }

    fn service_label(state: ServiceViewState, language: Language) -> &'static str {
        match state {
            ServiceViewState::Missing => language.text("Missing", "Не установлена"),
            ServiceViewState::Stopped => language.text("Stopped", "Остановлена"),
            ServiceViewState::Running => language.text("Running", "Работает"),
            ServiceViewState::StartPending => language.text("Starting", "Запускается"),
            ServiceViewState::StopPending => language.text("Stopping", "Останавливается"),
            ServiceViewState::Other => language.text("Other", "Другое"),
            ServiceViewState::Error => language.text("Unavailable", "Недоступна"),
        }
    }

    fn priority_label(priority: Option<u32>) -> String {
        priority.map_or_else(
            || "—".to_owned(),
            |value| match value {
                value if value == REALTIME_PRIORITY_CLASS.0 => "Realtime".to_owned(),
                value if value == HIGH_PRIORITY_CLASS.0 => "High".to_owned(),
                value if value == ABOVE_NORMAL_PRIORITY_CLASS.0 => "Above Normal".to_owned(),
                value if value == NORMAL_PRIORITY_CLASS.0 => "Normal".to_owned(),
                value if value == BELOW_NORMAL_PRIORITY_CLASS.0 => "Below Normal".to_owned(),
                value if value == IDLE_PRIORITY_CLASS.0 => "Idle".to_owned(),
                _ => format!("0x{value:X}"),
            },
        )
    }

    fn eco_qos_label(value: Option<ProcessEcoQosState>) -> &'static str {
        match value {
            Some(ProcessEcoQosState::Enabled) => "Enabled",
            Some(ProcessEcoQosState::Disabled) => "Disabled",
            Some(ProcessEcoQosState::Unset) => "Unset",
            None => "—",
        }
    }

    fn memory_priority_label(value: Option<ProcessMemoryPriority>) -> &'static str {
        match value {
            Some(ProcessMemoryPriority::VeryLow) => "Very Low",
            Some(ProcessMemoryPriority::Low) => "Low",
            Some(ProcessMemoryPriority::Medium) => "Medium",
            Some(ProcessMemoryPriority::BelowNormal) => "Below Normal",
            Some(ProcessMemoryPriority::Normal) => "Normal",
            None => "—",
        }
    }

    fn launch_settings(path: &Path, rule_image: Option<&str>) -> Result<(), String> {
        if !path.is_file() {
            return Err(format!(
                "Settings application is missing: {}",
                path.display()
            ));
        }
        let parameters = rule_image.map(|image| {
            let mut value = OsString::from("--rule-image \"");
            value.push(image);
            value.push("\"");
            value
        });
        shell_execute("runas", path, parameters.as_deref(), path.parent())
    }

    fn shell_execute(
        operation: &str,
        target: &Path,
        parameters: Option<&OsStr>,
        directory: Option<&Path>,
    ) -> Result<(), String> {
        let operation = operation.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let target = target
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let parameters =
            parameters.map(|value| value.encode_wide().chain(Some(0)).collect::<Vec<_>>());
        let directory = directory.map(|value| {
            value
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>()
        });
        // SAFETY: All optional and required UTF-16 buffers are NUL-terminated and remain live.
        let result = unsafe {
            ShellExecuteW(
                None,
                PCWSTR(operation.as_ptr()),
                PCWSTR(target.as_ptr()),
                parameters
                    .as_ref()
                    .map_or(PCWSTR::null(), |value| PCWSTR(value.as_ptr())),
                directory
                    .as_ref()
                    .map_or(PCWSTR::null(), |value| PCWSTR(value.as_ptr())),
                SW_SHOWNORMAL,
            )
        };
        if result.0 as isize > 32 {
            Ok(())
        } else {
            Err(format!(
                "ShellExecuteW failed with code {}",
                result.0 as isize
            ))
        }
    }

    fn activation_event_name() -> Result<Vec<u16>, String> {
        let session =
            winsched::platform::current_session_id().map_err(|error| error.to_string())?;
        Ok(format!("Local\\WinSchedMonitorActivate-{session}")
            .encode_utf16()
            .chain(Some(0))
            .collect())
    }

    fn signal_existing_instance() -> Result<(), String> {
        let name = activation_event_name()?;
        for _ in 0..20 {
            // SAFETY: The event name is NUL-terminated and access is limited to signalling.
            if let Ok(event) =
                unsafe { OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(name.as_ptr())) }
            {
                // SAFETY: event is a valid opened event handle.
                let result = unsafe { SetEvent(event) };
                // SAFETY: event is owned by this invocation and closed exactly once.
                unsafe {
                    let _ = CloseHandle(event);
                }
                return result.map_err(|error| error.to_string());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err("existing Process Monitor did not publish its activation event".to_owned())
    }

    fn start_activation_listener(context: egui::Context) -> Result<(), String> {
        let name = activation_event_name()?;
        // SAFETY: The event name is NUL-terminated; auto-reset prevents stale repeated requests.
        let event = unsafe { CreateEventW(None, false, false, PCWSTR(name.as_ptr())) }
            .map_err(|error| error.to_string())?;
        let event_value = event.0 as usize;
        std::thread::Builder::new()
            .name("winsched-monitor-activation".to_owned())
            .spawn(move || {
                let event = OwnedHandle(HANDLE(event_value as *mut core::ffi::c_void));
                loop {
                    // SAFETY: The owned event remains valid for the complete wait.
                    unsafe {
                        WaitForSingleObject(event.0, INFINITE);
                    }
                    context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    context.send_viewport_cmd(egui::ViewportCommand::Focus);
                    context.request_repaint();
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn detect_language() -> Language {
        for variable in ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"] {
            if std::env::var(variable)
                .is_ok_and(|value| value.to_ascii_lowercase().starts_with("ru"))
            {
                return Language::Russian;
            }
        }
        let mut locale = [0u16; 85];
        // SAFETY: The buffer is writable and meets LOCALE_NAME_MAX_LENGTH.
        let length = unsafe { GetUserDefaultLocaleName(&mut locale) };
        if length > 0
            && String::from_utf16_lossy(&locale[..usize::try_from(length).unwrap_or(0)])
                .to_ascii_lowercase()
                .starts_with("ru")
        {
            Language::Russian
        } else {
            Language::English
        }
    }

    pub(super) fn show_startup_error(message: &str) {
        let text = format!(
            "WinSched Process Monitor could not start. / Не удалось запустить монитор процессов WinSched.\n\n{message}"
        )
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
        // SAFETY: Both UTF-16 strings are NUL-terminated and live for the call.
        unsafe {
            MessageBoxW(
                None,
                PCWSTR(text.as_ptr()),
                w!("WinSched Process Monitor / Монитор процессов WinSched"),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    fn unix_time_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| {
                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
            })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use winsched_config::{RuleMode, WorkloadProfile};

        #[test]
        fn cpu_percent_uses_one_core_capacity_and_rejects_regression() {
            assert_eq!(
                cpu_basis_points(10_000_000, 15_000_000, Duration::from_secs(1)),
                Some(5_000)
            );
            assert_eq!(
                cpu_basis_points(15_000_000, 10_000_000, Duration::from_secs(1)),
                None
            );
        }

        #[test]
        fn exact_rule_label_takes_precedence() {
            let config = ControllerConfig {
                rules: vec![ProcessRule {
                    image: "worker.exe".to_owned(),
                    mode: RuleMode::Auto,
                    profile: WorkloadProfile::Memory,
                    group: None,
                    llc: None,
                }],
                ..ControllerConfig::default()
            };
            assert_eq!(
                process_scope_label(Some(&config), config.rules.first(), "worker.exe", None),
                "Exact: Auto/Memory"
            );
        }

        #[test]
        fn process_sampling_requires_a_focused_visible_window() {
            assert!(sampling_active(Some(true), Some(false), Some(false)));
            assert!(!sampling_active(Some(false), Some(false), Some(false)));
            assert!(!sampling_active(Some(true), Some(true), Some(false)));
            assert!(!sampling_active(Some(true), Some(false), Some(true)));
            assert!(!sampling_active(None, Some(false), Some(false)));
        }
    }
}
