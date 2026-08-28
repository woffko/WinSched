#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod elevation;

#[cfg(not(windows))]
fn main() {
    eprintln!("winsched-tray is only available on Windows");
}

#[cfg(windows)]
fn main() {
    if let Err(error) = app::run() {
        app::record_error(&format!("tray failed: {error}"));
    }
}

#[cfg(windows)]
mod app {
    #![allow(unsafe_code)] // Narrow ShellExecuteW calls are documented at each use.

    use std::error::Error;
    use std::ffi::{OsStr, OsString};
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use tray_icon::menu::{AboutMetadataBuilder, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::PCWSTR;
    use windows_service::Error as WindowsServiceError;
    use windows_service::service::{ServiceAccess, ServiceState, UserEventCode};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::window::WindowId;
    use winsched_control::{
        CONFIG_FILE_NAME, CONTROL_DISABLE, CONTROL_ENABLE, ControllerStatus,
        INSTALL_DIRECTORY_NAME, INTERACTIVE_PIPE_NAME, INTERACTIVE_STATE_HEARTBEAT_MS,
        INTERACTIVE_STATE_SCHEMA_VERSION, InteractiveActivityState, LOG_FILE_NAME, SERVICE_NAME,
        STATUS_FILE_NAME, STATUS_SCHEMA_VERSION,
    };
    use winsched_tray::{GITHUB_URL, MenuModel, ServiceViewState, about_details, build_menu_model};

    const MENU_SCHEDULING: &str = "scheduling";
    const MENU_SERVICE: &str = "service";
    const MENU_SETTINGS: &str = "settings";
    const MENU_OPEN_CONFIG: &str = "open-config";
    const MENU_OPEN_LOGS: &str = "open-logs";
    const MENU_REFRESH: &str = "refresh";
    const MENU_GITHUB: &str = "github";
    const MENU_EXIT: &str = "exit";
    const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
    const INTERACTIVE_PROBE_INTERVAL: Duration = Duration::from_millis(250);
    const STATUS_STALE_AFTER_MS: u64 = 75_000;

    #[derive(Debug, Clone)]
    enum UserEvent {
        Menu(MenuEvent),
        Tray(TrayIconEvent),
    }

    #[derive(Debug, Clone)]
    struct Paths {
        install_dir: PathBuf,
        config: PathBuf,
        log: PathBuf,
        status: PathBuf,
        settings: PathBuf,
        monitor: PathBuf,
    }

    struct InstanceLock {
        _file: fs::File,
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
            Self {
                config: install_dir.join(CONFIG_FILE_NAME),
                log: install_dir.join(LOG_FILE_NAME),
                status: install_dir.join(STATUS_FILE_NAME),
                settings: binary_dir.join("winsched-settings.exe"),
                monitor: binary_dir.join("winsched-monitor.exe"),
                install_dir,
            }
        }
    }

    struct InteractivePublisher {
        stop: Arc<AtomicBool>,
        worker: Option<JoinHandle<()>>,
    }

    impl InteractivePublisher {
        fn start() -> Result<Self, String> {
            let source = winsched::platform::current_process_key()
                .map_err(|error| format!("interactive source identity failed: {error}"))?;

            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = Arc::clone(&stop);
            let worker = std::thread::Builder::new()
                .name("winsched-interactive-probe".to_owned())
                .spawn(move || {
                    let mut last_error = None::<String>;
                    let mut last_published = None::<InteractiveActivityState>;
                    let mut last_heartbeat = Instant::now()
                        .checked_sub(Duration::from_millis(INTERACTIVE_STATE_HEARTBEAT_MS))
                        .unwrap_or_else(Instant::now);
                    while !worker_stop.load(Ordering::Acquire) {
                        let result = winsched::platform::capture_interactive_activity()
                            .map_err(|error| error.to_string())
                            .and_then(|activity| {
                                let state = interactive_state(activity, source);
                                let changed = last_published.as_ref().is_none_or(|previous| {
                                    !same_interactive_state_content(previous, &state)
                                });
                                let heartbeat_due = last_heartbeat.elapsed()
                                    >= Duration::from_millis(INTERACTIVE_STATE_HEARTBEAT_MS);
                                if changed || heartbeat_due {
                                    publish_interactive_state(&state)
                                        .map_err(|error| error.to_string())?;
                                    last_published = Some(state);
                                    last_heartbeat = Instant::now();
                                }
                                Ok(())
                            });
                        match result {
                            Ok(()) => last_error = None,
                            Err(error) if last_error.as_deref() == Some(error.as_str()) => {}
                            Err(error) => {
                                record_error(&format!("interactive probe failed: {error}"));
                                last_error = Some(error);
                            }
                        }
                        std::thread::sleep(INTERACTIVE_PROBE_INTERVAL);
                    }
                })
                .map_err(|error| format!("cannot start interactive probe thread: {error}"))?;
            Ok(Self {
                stop,
                worker: Some(worker),
            })
        }
    }

    impl Drop for InteractivePublisher {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn interactive_state(
        activity: winsched::platform::InteractiveActivity,
        source: winsched_core::adaptive::ProcessKey,
    ) -> InteractiveActivityState {
        let mut visible_pids = activity.visible_pids;
        visible_pids.sort_unstable();
        visible_pids.dedup();
        let mut audible_pids = activity.audible_pids;
        audible_pids.sort_unstable();
        audible_pids.dedup();
        InteractiveActivityState {
            schema_version: INTERACTIVE_STATE_SCHEMA_VERSION,
            session_id: activity.session_id,
            source_pid: source.pid,
            source_creation_time_100ns: source.creation_time_100ns,
            window_probe_available: activity.window_probe_available,
            audio_probe_available: activity.audio_probe_available,
            foreground_pid: activity.foreground_pid,
            visible_pids,
            audible_pids,
            updated_at_unix_ms: unix_time_ms(),
        }
    }

    fn same_interactive_state_content(
        left: &InteractiveActivityState,
        right: &InteractiveActivityState,
    ) -> bool {
        left.session_id == right.session_id
            && left.source_pid == right.source_pid
            && left.source_creation_time_100ns == right.source_creation_time_100ns
            && left.window_probe_available == right.window_probe_available
            && left.audio_probe_available == right.audio_probe_available
            && left.foreground_pid == right.foreground_pid
            && left.visible_pids == right.visible_pids
            && left.audible_pids == right.audible_pids
    }

    fn publish_interactive_state(
        state: &InteractiveActivityState,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let encoded = serde_json::to_vec(state)?;
        let mut output = OpenOptions::new().write(true).open(INTERACTIVE_PIPE_NAME)?;
        output.write_all(&encoded)?;
        Ok(())
    }

    struct TrayUi {
        tray: TrayIcon,
        header: MenuItem,
        scheduling: MenuItem,
        service: MenuItem,
        mode: MenuItem,
        managed: MenuItem,
        reserve: MenuItem,
        latency: MenuItem,
        background: MenuItem,
        activity: MenuItem,
        error: MenuItem,
        settings: MenuItem,
        open_config: MenuItem,
        open_logs: MenuItem,
    }

    impl TrayUi {
        fn new() -> Result<Self, Box<dyn Error>> {
            let menu = Menu::new();
            let header = MenuItem::new("WinSched — Loading...", false, None);
            let scheduling = MenuItem::with_id(MENU_SCHEDULING, "Enable Scheduling", false, None);
            let service = MenuItem::with_id(MENU_SERVICE, "Service Unavailable", false, None);
            let mode = MenuItem::new("Mode: Unknown", false, None);
            let managed = MenuItem::new("Managed processes: 0", false, None);
            let reserve = MenuItem::new("System reserve: unavailable", false, None);
            let latency = MenuItem::new("Latency guard: unavailable", false, None);
            let background = MenuItem::new("Background QoS: unavailable", false, None);
            let activity = MenuItem::new("Last activity: none", false, None);
            let error = MenuItem::new("Last error: none", false, None);
            let settings = MenuItem::with_id(MENU_SETTINGS, "Settings...", true, None);
            let open_config = MenuItem::with_id(
                MENU_OPEN_CONFIG,
                "Open Configuration (Advanced)",
                true,
                None,
            );
            let open_logs = MenuItem::with_id(MENU_OPEN_LOGS, "Open Logs", true, None);
            let refresh = MenuItem::with_id(MENU_REFRESH, "Refresh Status", true, None);
            let details = about_details(env!("CARGO_PKG_VERSION"));
            let metadata = AboutMetadataBuilder::new()
                .name(Some(details.name))
                .version(Some(details.version))
                .comments(Some(details.comments))
                .license(Some(details.license))
                .website(Some(details.website))
                .website_label(Some(details.website_label))
                .build();
            let about = PredefinedMenuItem::about(Some("About WinSched..."), Some(metadata));
            let github = MenuItem::with_id(MENU_GITHUB, "GitHub Repository", true, None);
            let exit = MenuItem::with_id(MENU_EXIT, "Exit Tray", true, None);
            let separator_1 = PredefinedMenuItem::separator();
            let separator_2 = PredefinedMenuItem::separator();
            let separator_3 = PredefinedMenuItem::separator();
            let separator_4 = PredefinedMenuItem::separator();

            menu.append_items(&[
                &header,
                &scheduling,
                &service,
                &separator_1,
                &mode,
                &managed,
                &reserve,
                &latency,
                &background,
                &activity,
                &error,
                &separator_2,
                &settings,
                &open_config,
                &open_logs,
                &refresh,
                &separator_3,
                &about,
                &github,
                &separator_4,
                &exit,
            ])?;

            let icon = load_icon()?;
            let tray = TrayIconBuilder::new()
                .with_id("winsched-tray")
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_menu_on_right_click(true)
                .with_tooltip("WinSched: loading service status")
                .with_icon(icon)
                .build()?;

            Ok(Self {
                tray,
                header,
                scheduling,
                service,
                mode,
                managed,
                reserve,
                latency,
                background,
                activity,
                error,
                settings,
                open_config,
                open_logs,
            })
        }

        fn update(&self, model: &MenuModel, data_present: bool, settings_present: bool) {
            self.header.set_text(&model.header);
            self.scheduling.set_text(&model.scheduling_action);
            self.scheduling.set_enabled(model.scheduling_action_enabled);
            self.service.set_text(&model.service_action);
            self.service.set_enabled(model.service_action_enabled);
            self.mode.set_text(&model.mode);
            self.managed.set_text(&model.managed);
            self.reserve.set_text(&model.reserve);
            self.latency.set_text(&model.latency);
            self.background.set_text(&model.background);
            self.activity.set_text(&model.activity);
            self.error.set_text(&model.error);
            self.settings.set_enabled(settings_present);
            self.open_config.set_enabled(data_present);
            self.open_logs.set_enabled(data_present);
            let _ = self.tray.set_tooltip(Some(&model.tooltip));
        }
    }

    struct TrayApplication {
        paths: Paths,
        ui: Option<TrayUi>,
        next_refresh: Instant,
        last_service: ServiceViewState,
        last_status: Option<ControllerStatus>,
        action_error: Option<String>,
    }

    impl TrayApplication {
        fn new(paths: Paths) -> Self {
            Self {
                paths,
                ui: None,
                next_refresh: Instant::now(),
                last_service: ServiceViewState::Error("not queried yet".to_owned()),
                last_status: None,
                action_error: None,
            }
        }

        fn refresh(&mut self) {
            self.last_service = query_service_state();
            let (status, status_error) = read_controller_status(&self.paths.status);
            self.last_status = status;
            let stale_error = self.last_status.as_ref().and_then(|status| {
                (matches!(self.last_service, ServiceViewState::Running)
                    && unix_time_ms().saturating_sub(status.updated_at_unix_ms)
                        > STATUS_STALE_AFTER_MS)
                    .then(|| "Service status heartbeat is stale".to_owned())
            });
            let visible_error = self
                .action_error
                .as_deref()
                .or(status_error.as_deref())
                .or(stale_error.as_deref());
            let model =
                build_menu_model(&self.last_service, self.last_status.as_ref(), visible_error);
            if let Some(ui) = &self.ui {
                ui.update(
                    &model,
                    self.paths.install_dir.exists(),
                    self.paths.settings.exists(),
                );
            }
            self.next_refresh = Instant::now() + REFRESH_INTERVAL;
        }

        fn handle_menu(&mut self, event_loop: &ActiveEventLoop, event: &MenuEvent) {
            let result = if event.id == MENU_SCHEDULING {
                let enabled = self
                    .last_status
                    .as_ref()
                    .is_some_and(|status| status.scheduling_enabled);
                set_scheduling(!enabled)
            } else if event.id == MENU_SERVICE {
                change_service_state(&self.last_service)
            } else if event.id == MENU_SETTINGS {
                launch_settings(&self.paths.settings)
            } else if event.id == MENU_OPEN_CONFIG {
                open_file_or_directory(&self.paths.config, &self.paths.install_dir)
            } else if event.id == MENU_OPEN_LOGS {
                open_file_or_directory(&self.paths.log, &self.paths.install_dir)
            } else if event.id == MENU_REFRESH {
                Ok(())
            } else if event.id == MENU_GITHUB {
                open_url(GITHUB_URL)
            } else if event.id == MENU_EXIT {
                event_loop.exit();
                return;
            } else {
                return;
            };

            match result {
                Ok(()) => self.action_error = None,
                Err(error) => {
                    record_error(&error);
                    self.action_error = Some(error);
                }
            }
            self.refresh();
        }

        fn handle_tray(&mut self, event: &TrayIconEvent) {
            let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            else {
                return;
            };
            match launch_monitor(&self.paths.monitor) {
                Ok(()) => self.action_error = None,
                Err(error) => {
                    record_error(&error);
                    self.action_error = Some(error);
                }
            }
            self.refresh();
        }
    }

    impl ApplicationHandler<UserEvent> for TrayApplication {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.ui.is_none() {
                match TrayUi::new() {
                    Ok(ui) => self.ui = Some(ui),
                    Err(error) => {
                        record_error(&format!("failed to create tray UI: {error}"));
                        event_loop.exit();
                        return;
                    }
                }
            }
            self.refresh();
        }

        fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
            match event {
                UserEvent::Menu(event) => self.handle_menu(event_loop, &event),
                UserEvent::Tray(event) => self.handle_tray(&event),
            }
        }

        fn window_event(
            &mut self,
            _event_loop: &ActiveEventLoop,
            _window_id: WindowId,
            _event: WindowEvent,
        ) {
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            if Instant::now() >= self.next_refresh {
                self.refresh();
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_refresh));
        }
    }

    pub(super) fn run() -> Result<(), Box<dyn Error>> {
        if crate::elevation::is_elevated()? {
            record_error("tray refused to run with an elevated token; use the Startup shortcut");
            return Ok(());
        }
        let Some(instance_lock) = acquire_instance_lock()? else {
            return Ok(());
        };

        let paths = Paths::discover();
        let interactive_publisher = match InteractivePublisher::start() {
            Ok(publisher) => Some(publisher),
            Err(error) => {
                record_error(&error);
                None
            }
        };

        let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
        let proxy = event_loop.create_proxy();
        MenuEvent::set_event_handler(Some(move |event| {
            let _ = proxy.send_event(UserEvent::Menu(event));
        }));
        let tray_proxy = event_loop.create_proxy();
        TrayIconEvent::set_event_handler(Some(move |event| {
            let _ = tray_proxy.send_event(UserEvent::Tray(event));
        }));
        let mut application = TrayApplication::new(paths);
        event_loop.run_app(&mut application)?;
        drop(interactive_publisher);
        drop(instance_lock);
        Ok(())
    }

    fn acquire_instance_lock() -> Result<Option<InstanceLock>, std::io::Error> {
        let directory = tray_state_directory();
        fs::create_dir_all(&directory)?;
        let session = winsched::platform::current_session_id()
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let path = directory.join(format!("tray-{session}.lock"));
        acquire_lock_at(&path)
    }

    fn acquire_lock_at(path: &Path) -> Result<Option<InstanceLock>, std::io::Error> {
        match OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .share_mode(0)
            .open(path)
        {
            Ok(file) => Ok(Some(InstanceLock { _file: file })),
            Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn load_icon() -> Result<Icon, Box<dyn Error>> {
        let image = image::load_from_memory_with_format(
            include_bytes!("../../../assets/tray/winsched-tray-64.png"),
            image::ImageFormat::Png,
        )?
        .into_rgba8();
        let (width, height) = image.dimensions();
        Ok(Icon::from_rgba(image.into_raw(), width, height)?)
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
                state => ServiceViewState::Other(format!("{state:?}")),
            },
            Err(error) if is_service_missing(&error) => ServiceViewState::Missing,
            Err(error) => ServiceViewState::Error(format_service_error(&error)),
        }
    }

    fn read_controller_status(path: &Path) -> (Option<ControllerStatus>, Option<String>) {
        if !path.exists() {
            return (None, None);
        }
        match fs::read(path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                serde_json::from_slice::<ControllerStatus>(&bytes)
                    .map_err(|error| error.to_string())
            }) {
            Ok(status) if status.schema_version == STATUS_SCHEMA_VERSION => (Some(status), None),
            Ok(status) => (
                None,
                Some(format!(
                    "Unsupported status schema {}; expected {STATUS_SCHEMA_VERSION}",
                    status.schema_version
                )),
            ),
            Err(error) => (None, Some(format!("Cannot read service status: {error}"))),
        }
    }

    fn change_service_state(current: &ServiceViewState) -> Result<(), String> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
            .map_err(|error| format_service_error(&error))?;
        match current {
            ServiceViewState::Running => {
                let service = manager
                    .open_service(
                        SERVICE_NAME,
                        ServiceAccess::STOP | ServiceAccess::QUERY_STATUS,
                    )
                    .map_err(|error| format_service_error(&error))?;
                service
                    .stop()
                    .map_err(|error| format_service_error(&error))?;
                Ok(())
            }
            ServiceViewState::Stopped => {
                let service = manager
                    .open_service(
                        SERVICE_NAME,
                        ServiceAccess::START | ServiceAccess::QUERY_STATUS,
                    )
                    .map_err(|error| format_service_error(&error))?;
                service
                    .start::<&OsStr>(&[])
                    .map_err(|error| format_service_error(&error))?;
                Ok(())
            }
            _ => Err("Service is not in a startable or stoppable state".to_owned()),
        }
    }

    fn set_scheduling(enabled: bool) -> Result<(), String> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
            .map_err(|error| format_service_error(&error))?;
        let service = manager
            .open_service(
                SERVICE_NAME,
                ServiceAccess::USER_DEFINED_CONTROL | ServiceAccess::QUERY_STATUS,
            )
            .map_err(|error| format_service_error(&error))?;
        let code = UserEventCode::from_raw(if enabled {
            CONTROL_ENABLE
        } else {
            CONTROL_DISABLE
        })
        .expect("WinSched control codes are in the documented 128..=255 range");
        service
            .notify(code)
            .map_err(|error| format_service_error(&error))?;
        Ok(())
    }

    fn open_file_or_directory(file: &Path, directory: &Path) -> Result<(), String> {
        if !file.exists() {
            return shell_execute("open", directory, None);
        }
        let notepad = system_notepad_path()?;
        let mut parameters = OsString::from("\"");
        parameters.push(file.as_os_str());
        parameters.push("\"");
        shell_execute_with_parameters(
            "open",
            &notepad,
            Some(parameters.as_os_str()),
            notepad.parent(),
        )
    }

    fn open_url(url: &str) -> Result<(), String> {
        shell_execute("open", Path::new(url), None)
    }

    fn launch_settings(path: &Path) -> Result<(), String> {
        if !path.is_file() {
            return Err(format!(
                "Settings application is missing: {}",
                path.display()
            ));
        }
        shell_execute("runas", path, path.parent())
    }

    fn launch_monitor(path: &Path) -> Result<(), String> {
        if !path.is_file() {
            return Err(format!(
                "Process Monitor application is missing: {}",
                path.display()
            ));
        }
        Command::new(path)
            .current_dir(path.parent().unwrap_or_else(|| Path::new(".")))
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("cannot start Process Monitor: {error}"))
    }

    fn shell_execute(
        operation: &str,
        target: &Path,
        directory: Option<&Path>,
    ) -> Result<(), String> {
        shell_execute_with_parameters(operation, target, None, directory)
    }

    fn shell_execute_with_parameters(
        operation: &str,
        target: &Path,
        parameters: Option<&OsStr>,
        directory: Option<&Path>,
    ) -> Result<(), String> {
        let operation = operation.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let target_wide = target
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let directory_wide = directory.map(|directory| {
            directory
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>()
        });
        let parameters_wide = parameters
            .map(|parameters| parameters.encode_wide().chain(Some(0)).collect::<Vec<_>>());
        // SAFETY: All UTF-16 buffers are NUL-terminated and remain live for the call.
        let result = unsafe {
            ShellExecuteW(
                None,
                PCWSTR(operation.as_ptr()),
                PCWSTR(target_wide.as_ptr()),
                parameters_wide
                    .as_ref()
                    .map_or(PCWSTR::null(), |value| PCWSTR(value.as_ptr())),
                directory_wide
                    .as_ref()
                    .map_or(PCWSTR::null(), |value| PCWSTR(value.as_ptr())),
                SW_SHOWNORMAL,
            )
        };
        let code = result.0 as isize;
        if code > 32 {
            Ok(())
        } else {
            Err(format!(
                "ShellExecuteW failed for {} with code {code}",
                target.display()
            ))
        }
    }

    fn system_notepad_path() -> Result<PathBuf, String> {
        // SAFETY: A null output buffer asks Windows for the required UTF-16 length.
        let required = unsafe { GetSystemDirectoryW(None) };
        if required == 0 {
            return Err(format!(
                "GetSystemDirectoryW failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut buffer = vec![0u16; usize::try_from(required).expect("u32 fits usize") + 1];
        // SAFETY: The buffer was sized from the preceding Win32 query.
        let written = unsafe { GetSystemDirectoryW(Some(buffer.as_mut_slice())) };
        let written = usize::try_from(written).expect("u32 fits usize");
        if written == 0 || written >= buffer.len() {
            return Err(format!(
                "GetSystemDirectoryW failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        buffer.truncate(written);
        Ok(PathBuf::from(OsString::from_wide(&buffer)).join("notepad.exe"))
    }

    fn is_service_missing(error: &WindowsServiceError) -> bool {
        matches!(
            error,
            WindowsServiceError::Winapi(error) if error.raw_os_error() == Some(1060)
        )
    }

    fn format_service_error(error: &WindowsServiceError) -> String {
        match error {
            WindowsServiceError::Winapi(source) => format!("Windows service error: {source}"),
            _ => format!("Windows service error: {error}"),
        }
    }

    pub(super) fn record_error(message: &str) {
        let base = tray_state_directory();
        if fs::create_dir_all(&base).is_err() {
            return;
        }
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(base.join("tray.log"))
        {
            let _ = writeln!(file, "{} {message}", unix_time_ms());
        }
    }

    fn tray_state_directory() -> PathBuf {
        std::env::var_os("LOCALAPPDATA")
            .map_or_else(std::env::temp_dir, PathBuf::from)
            .join("WinSched")
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

        #[test]
        fn instance_lock_is_exclusive_and_released_on_drop() {
            let path = std::env::temp_dir().join(format!(
                "winsched-tray-lock-{}-{}.lock",
                std::process::id(),
                unix_time_ms()
            ));
            let first = acquire_lock_at(&path).unwrap().unwrap();
            assert!(acquire_lock_at(&path).unwrap().is_none());
            drop(first);
            assert!(acquire_lock_at(&path).unwrap().is_some());
            fs::remove_file(path).unwrap();
        }
    }
}
