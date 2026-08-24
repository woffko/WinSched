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
    use std::error::Error;
    use std::ffi::OsStr;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use tray_icon::menu::{AboutMetadataBuilder, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
    use windows_service::Error as WindowsServiceError;
    use windows_service::service::{ServiceAccess, ServiceState, UserEventCode};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::window::WindowId;
    use winsched_control::{
        CONFIG_FILE_NAME, CONTROL_DISABLE, CONTROL_ENABLE, ControllerStatus,
        INSTALL_DIRECTORY_NAME, LOG_FILE_NAME, SERVICE_NAME, STATUS_FILE_NAME,
        STATUS_SCHEMA_VERSION,
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
    const STATUS_STALE_AFTER_MS: u64 = 75_000;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    #[derive(Debug, Clone)]
    enum UserEvent {
        Menu(MenuEvent),
    }

    #[derive(Debug, Clone)]
    struct Paths {
        install_dir: PathBuf,
        config: PathBuf,
        log: PathBuf,
        status: PathBuf,
        settings: PathBuf,
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
                install_dir,
            }
        }
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
                .with_menu_on_left_click(true)
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
                open_file_or_directory(&self.paths.config, &self.paths.install_dir, "notepad.exe")
            } else if event.id == MENU_OPEN_LOGS {
                open_file_or_directory(&self.paths.log, &self.paths.install_dir, "notepad.exe")
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

        let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
        let proxy = event_loop.create_proxy();
        MenuEvent::set_event_handler(Some(move |event| {
            let _ = proxy.send_event(UserEvent::Menu(event));
        }));
        let mut application = TrayApplication::new(Paths::discover());
        event_loop.run_app(&mut application)?;
        drop(instance_lock);
        Ok(())
    }

    fn acquire_instance_lock() -> Result<Option<InstanceLock>, std::io::Error> {
        let directory = tray_state_directory();
        fs::create_dir_all(&directory)?;
        let session = std::env::var("SESSIONNAME").unwrap_or_else(|_| "default".to_owned());
        let session = session
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
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

    fn open_file_or_directory(file: &Path, directory: &Path, editor: &str) -> Result<(), String> {
        let (program, argument) = if file.exists() {
            (editor, file)
        } else {
            ("explorer.exe", directory)
        };
        Command::new(program)
            .arg(argument)
            .spawn()
            .map_err(|error| format!("Cannot open {}: {error}", argument.display()))?;
        Ok(())
    }

    fn open_url(url: &str) -> Result<(), String> {
        Command::new("explorer.exe")
            .arg(url)
            .spawn()
            .map_err(|error| format!("Cannot open {url}: {error}"))?;
        Ok(())
    }

    fn launch_settings(path: &Path) -> Result<(), String> {
        if !path.is_file() {
            return Err(format!(
                "Settings application is missing: {}",
                path.display()
            ));
        }
        let escaped_path = path.to_string_lossy().replace('\'', "''");
        let script = format!("Start-Process -FilePath '{escaped_path}' -Verb RunAs");
        let status = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                &script,
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map_err(|error| format!("Cannot launch Settings: {error}"))?;
        if !status.success() {
            return Err(format!(
                "Settings launch was cancelled or failed with {status}"
            ));
        }
        Ok(())
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
