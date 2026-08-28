#![allow(unsafe_code)] // Narrow Win32 UI and locale calls are documented at each use.

use std::error::Error;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui::{self, Color32, RichText};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Globalization::GetUserDefaultLocaleName;
use windows::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::PCWSTR;
use winsched::diagnostics::{
    self, DiagnosticFindingCode, DiagnosticOptions, DiagnosticReport, DiagnosticSeverity,
};
use winsched_config::{
    CONFIG_SCHEMA_VERSION, ControllerConfig, ControllerMode, LoggingConfig, LoggingLevel,
    MAX_CONFIGURED_PHYSICAL_CORES, MAX_LATENCY_THRESHOLD_US, MAX_LOG_FILE_SIZE_MIB,
    MAX_MEMORY_RESIZE_COOLDOWN_MS, MAX_RESPONSIVENESS_STABILITY_SAMPLES, MAX_RETAINED_LOG_ARCHIVES,
    MAX_SYSTEM_RESERVE_PERCENT, MIN_LATENCY_THRESHOLD_US, MIN_LOG_FILE_SIZE_MIB,
    MIN_MEMORY_RESIZE_COOLDOWN_MS, MIN_SYSTEM_RESERVE_PERCENT, ProcessRule, RuleMode,
    WorkloadProfile,
};
use winsched_control::{ConfigReloadResult, ControllerStatus, STATUS_SCHEMA_VERSION};
use winsched_core::responsiveness::ResponsivenessPressure;
use winsched_settings::{
    SettingsPaths, config_reload_wait_ms, load_config, restore_defaults, save_config_atomic,
    set_tray_autostart, tray_autostart_enabled,
};

const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(250);
const SETTINGS_REQUEST_SCHEMA_VERSION: u32 = 1;
const SETTINGS_REQUEST_MAX_AGE_MS: u64 = 60_000;

pub fn run() -> Result<(), Box<dyn Error>> {
    let launch_request = parse_launch_request()?;
    let paths = SettingsPaths::discover();
    let request_path = settings_request_path();
    let Some(instance) = InstanceLock::try_acquire(&paths.instance_lock)? else {
        if let Some(request) = launch_request {
            write_activation_request(&request_path, &request)?;
        }
        signal_existing_instance()?;
        return Ok(());
    };
    let config = load_config(&paths.config)?;
    let language = detect_language();
    let title = language.text("WinSched Settings", "Настройки WinSched");
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([980.0, 720.0])
            .with_min_inner_size([840.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        title,
        native_options,
        Box::new(move |context| {
            context.egui_ctx.set_zoom_factor(1.05);
            let activation =
                start_activation_listener(context.egui_ctx.clone()).map_err(|error| {
                    Box::<dyn Error + Send + Sync>::from(std::io::Error::other(error))
                })?;
            Ok(Box::new(SettingsApp::new(
                paths,
                config,
                language,
                launch_request,
                request_path,
                activation,
            )))
        }),
    )?;
    drop(instance);
    Ok(())
}

pub fn show_startup_error(message: &str) {
    let detail = format!(
        "WinSched Settings could not start. / Не удалось запустить настройки WinSched.\n\n{message}"
    );
    let text = detail.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let caption = "WinSched Settings / Настройки WinSched"
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: Both UTF-16 buffers are NUL-terminated and live for the complete call.
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsActivationRequest {
    schema_version: u32,
    rule_image: String,
    created_at_unix_ms: u64,
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

fn parse_launch_request() -> Result<Option<SettingsActivationRequest>, std::io::Error> {
    let mut arguments = std::env::args_os().skip(1);
    let Some(argument) = arguments.next() else {
        return Ok(None);
    };
    if argument != "--rule-image" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unsupported Settings command-line argument",
        ));
    }
    let image = arguments.next().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "--rule-image requires one executable image name",
        )
    })?;
    if arguments.next().is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unexpected arguments after --rule-image",
        ));
    }
    Ok(Some(SettingsActivationRequest {
        schema_version: SETTINGS_REQUEST_SCHEMA_VERSION,
        rule_image: validate_rule_image(image)?,
        created_at_unix_ms: unix_time_ms(),
    }))
}

fn validate_rule_image(image: OsString) -> Result<String, std::io::Error> {
    let image = image.into_string().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "rule image name is not valid Unicode",
        )
    })?;
    let image = image.trim();
    if image.is_empty() || image.contains(['/', '\\']) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "rule image must be one executable file name without a path",
        ));
    }
    Ok(image.to_owned())
}

fn settings_request_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("WinSched")
        .join("settings-activation.json")
}

fn write_activation_request(
    path: &Path,
    request: &SettingsActivationRequest,
) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let encoded = serde_json::to_vec(request).map_err(std::io::Error::other)?;
    winsched::platform::atomic_replace_file(path, &encoded)
}

fn take_activation_request(path: &Path) -> Option<SettingsActivationRequest> {
    let request = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SettingsActivationRequest>(&bytes).ok());
    let _ = fs::remove_file(path);
    request.filter(|request| {
        request.schema_version == SETTINGS_REQUEST_SCHEMA_VERSION
            && request.created_at_unix_ms <= unix_time_ms()
            && unix_time_ms().saturating_sub(request.created_at_unix_ms)
                <= SETTINGS_REQUEST_MAX_AGE_MS
            && !request.rule_image.is_empty()
            && !request.rule_image.contains(['/', '\\'])
    })
}

fn settings_activation_event_name() -> Result<Vec<u16>, String> {
    let session = winsched::platform::current_session_id().map_err(|error| error.to_string())?;
    Ok(format!("Local\\WinSchedSettingsActivate-{session}")
        .encode_utf16()
        .chain(Some(0))
        .collect())
}

fn signal_existing_instance() -> Result<(), String> {
    let name = settings_activation_event_name()?;
    for _ in 0..20 {
        // SAFETY: The event name is NUL-terminated and access is limited to signalling.
        if let Ok(event) = unsafe { OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(name.as_ptr())) } {
            // SAFETY: event is a valid opened event handle.
            let result = unsafe { SetEvent(event) };
            // SAFETY: This invocation owns the opened handle.
            unsafe {
                let _ = CloseHandle(event);
            }
            return result.map_err(|error| error.to_string());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("existing Settings instance did not publish its activation event".to_owned())
}

fn start_activation_listener(context: egui::Context) -> Result<mpsc::Receiver<()>, String> {
    let name = settings_activation_event_name()?;
    // SAFETY: The event name is NUL-terminated; auto-reset coalesces activation requests.
    let event = unsafe { CreateEventW(None, false, false, PCWSTR(name.as_ptr())) }
        .map_err(|error| error.to_string())?;
    let event_value = event.0 as usize;
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("winsched-settings-activation".to_owned())
        .spawn(move || {
            let event = OwnedHandle(HANDLE(event_value as *mut core::ffi::c_void));
            loop {
                // SAFETY: The owned event remains valid for the complete wait.
                unsafe {
                    WaitForSingleObject(event.0, INFINITE);
                }
                if sender.send(()).is_err() {
                    break;
                }
                context.request_repaint();
            }
        })
        .map_err(|error| error.to_string())?;
    Ok(receiver)
}

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

fn detect_language() -> Language {
    for variable in ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"] {
        if std::env::var(variable).is_ok_and(|value| value.to_ascii_lowercase().starts_with("ru")) {
            return Language::Russian;
        }
    }
    let mut locale = [0u16; 85];
    // SAFETY: The fixed buffer is writable and meets LOCALE_NAME_MAX_LENGTH.
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

struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    fn try_acquire(path: &Path) -> std::io::Result<Option<Self>> {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    General,
    Adaptive,
    Responsiveness,
    BackgroundEfficiency,
    Rules,
    Logging,
    Diagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BannerKind {
    Information,
    Success,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceReloadState {
    InSync,
    RetryRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirmation {
    None,
    RestoreDefaults,
    CancelChanges,
    Close,
}

struct PendingDiagnostic {
    receiver: mpsc::Receiver<Result<DiagnosticReport, String>>,
    cancellation: Arc<AtomicBool>,
    started: Instant,
    cancelling: bool,
}

struct Banner {
    kind: BannerKind,
    message: String,
}

struct PendingReload {
    baseline: Option<(u32, u64)>,
    not_before_unix_ms: u64,
    expected_mode: ControllerMode,
    expected_config_fingerprint: u64,
    expected_logging: LoggingConfig,
    deadline: Instant,
    next_poll: Instant,
}

enum ReloadPollOutcome {
    Pending(Duration),
    Reloaded,
    Rejected(Option<String>),
    TimedOutWithoutReceipt,
    TimedOutAfterMismatchedReceipt,
}

impl PendingReload {
    fn poll(&mut self, paths: &SettingsPaths, now: Instant) -> ReloadPollOutcome {
        if now < self.next_poll {
            return ReloadPollOutcome::Pending(self.next_poll - now);
        }
        self.next_poll = now + STATUS_POLL_INTERVAL;
        let status = read_status(&paths.status);
        let current_receipt = status.as_ref().filter(|status| {
            status.is_reload_receipt_after(self.baseline, self.not_before_unix_ms)
        });
        if let Some(status) = current_receipt {
            match status.config_reload_result {
                ConfigReloadResult::Reloaded
                    if status.configured_mode == self.expected_mode
                        && status.applied_config_fingerprint
                            == self.expected_config_fingerprint
                        && status.applied_logging == self.expected_logging =>
                {
                    return ReloadPollOutcome::Reloaded;
                }
                ConfigReloadResult::Rejected => {
                    return ReloadPollOutcome::Rejected(status.config_reload_error.clone());
                }
                ConfigReloadResult::Initial | ConfigReloadResult::Reloaded => {}
            }
        }

        if now < self.deadline {
            return ReloadPollOutcome::Pending(STATUS_POLL_INTERVAL);
        }
        if current_receipt.is_some() {
            ReloadPollOutcome::TimedOutAfterMismatchedReceipt
        } else {
            ReloadPollOutcome::TimedOutWithoutReceipt
        }
    }
}

struct SettingsApp {
    paths: SettingsPaths,
    config: ControllerConfig,
    persisted: ControllerConfig,
    tray_autostart: bool,
    persisted_tray_autostart: bool,
    tab: SettingsTab,
    banner: Option<Banner>,
    confirmation: Confirmation,
    allow_close: bool,
    pending_reload: Option<PendingReload>,
    service_reload_state: ServiceReloadState,
    pending_diagnostic: Option<PendingDiagnostic>,
    diagnostic_report: Option<DiagnosticReport>,
    diagnostic_error: Option<String>,
    activation_request_path: PathBuf,
    activation_receiver: mpsc::Receiver<()>,
    rule_focus_image: Option<String>,
    language: Language,
}

impl SettingsApp {
    fn new(
        paths: SettingsPaths,
        config: ControllerConfig,
        language: Language,
        launch_request: Option<SettingsActivationRequest>,
        activation_request_path: PathBuf,
        activation_receiver: mpsc::Receiver<()>,
    ) -> Self {
        let tray_autostart = tray_autostart_enabled(&paths.tray_startup_shortcut);
        let mut app = Self {
            paths,
            persisted: config.clone(),
            config,
            tray_autostart,
            persisted_tray_autostart: tray_autostart,
            tab: SettingsTab::General,
            banner: Some(Banner {
                kind: BannerKind::Information,
                message: language
                    .text(
                        "Configuration loaded. Changes take effect after Apply.",
                        "Конфигурация загружена. Изменения вступят в силу после нажатия «Применить».",
                    )
                    .to_owned(),
            }),
            confirmation: Confirmation::None,
            allow_close: false,
            pending_reload: None,
            service_reload_state: ServiceReloadState::InSync,
            pending_diagnostic: None,
            diagnostic_report: None,
            diagnostic_error: None,
            activation_request_path,
            activation_receiver,
            rule_focus_image: None,
            language,
        };
        if let Some(request) = launch_request {
            app.open_rule_request(&request);
        }
        app
    }

    fn open_rule_request(&mut self, request: &SettingsActivationRequest) {
        let language = self.language;
        self.tab = SettingsTab::Rules;
        let existing = !ensure_exact_rule_draft(&mut self.config, &request.rule_image);
        self.rule_focus_image = Some(request.rule_image.clone());
        self.set_banner(
            BannerKind::Information,
            if existing {
                format!(
                    "{}: {}",
                    language.text("Existing exact rule opened", "Открыто существующее правило"),
                    request.rule_image
                )
            } else {
                format!(
                    "{}: {}. {}",
                    language.text(
                        "Exact-rule draft created",
                        "Создан черновик точного правила"
                    ),
                    request.rule_image,
                    language.text(
                        "Review it and choose Apply to save.",
                        "Проверьте его и нажмите «Применить» для сохранения."
                    )
                )
            },
        );
    }

    fn poll_activation(&mut self, context: &egui::Context) {
        let mut activated = false;
        while self.activation_receiver.try_recv().is_ok() {
            activated = true;
        }
        if !activated {
            return;
        }
        if let Some(request) = take_activation_request(&self.activation_request_path) {
            self.open_rule_request(&request);
        }
        context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        context.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn is_dirty(&self) -> bool {
        self.config != self.persisted || self.tray_autostart != self.persisted_tray_autostart
    }

    fn set_banner(&mut self, kind: BannerKind, message: impl Into<String>) {
        self.banner = Some(Banner {
            kind,
            message: message.into(),
        });
    }

    fn reload_from_disk(&mut self) {
        let language = self.language;
        match load_config(&self.paths.config) {
            Ok(config) => {
                self.persisted.clone_from(&config);
                self.config = config;
                self.tray_autostart = tray_autostart_enabled(&self.paths.tray_startup_shortcut);
                self.persisted_tray_autostart = self.tray_autostart;
                self.confirmation = Confirmation::None;
                self.pending_reload = None;
                self.set_banner(
                    BannerKind::Success,
                    language.text(
                        "Configuration reloaded from disk.",
                        "Конфигурация повторно загружена с диска.",
                    ),
                );
            }
            Err(error) => self.set_banner(
                BannerKind::Error,
                format!(
                    "{}: {error}",
                    language.text("Reload failed", "Ошибка повторной загрузки")
                ),
            ),
        }
    }

    fn report_config_save_failure(
        &mut self,
        error: &dyn std::fmt::Display,
        rollback_autostart: bool,
    ) {
        let language = self.language;
        let rollback_detail = rollback_autostart
            .then(|| {
                set_tray_autostart(
                    &self.paths.tray_shortcut,
                    &self.paths.tray_startup_shortcut,
                    self.persisted_tray_autostart,
                )
            })
            .and_then(Result::err)
            .map_or_else(String::new, |rollback_error| {
                format!(
                    "; {}: {rollback_error}",
                    language.text(
                        "tray autostart rollback also failed",
                        "откат автозапуска приложения в трее также завершился ошибкой"
                    )
                )
            });
        self.pending_reload = None;
        self.set_banner(
            BannerKind::Error,
            format!(
                "{}: {error}{rollback_detail}",
                language.text("Apply failed", "Ошибка применения"),
            ),
        );
    }

    fn apply(&mut self) {
        let language = self.language;
        let config_changed = self.config != self.persisted;
        let config_write_required =
            config_changed || self.service_reload_state == ServiceReloadState::RetryRequired;
        let autostart_changed = self.tray_autostart != self.persisted_tray_autostart;
        if let Err(error) = self.config.clone().validate() {
            self.set_banner(
                BannerKind::Error,
                format!(
                    "{}: {error}",
                    language.text("Validation failed", "Ошибка проверки")
                ),
            );
            return;
        }

        if autostart_changed
            && let Err(error) = set_tray_autostart(
                &self.paths.tray_shortcut,
                &self.paths.tray_startup_shortcut,
                self.tray_autostart,
            )
        {
            self.set_banner(
                BannerKind::Error,
                format!(
                    "{}: {error}",
                    language.text(
                        "Could not update tray autostart",
                        "Не удалось изменить автозапуск приложения в трее"
                    )
                ),
            );
            return;
        }

        if !config_write_required {
            self.persisted_tray_autostart = self.tray_autostart;
            self.confirmation = Confirmation::None;
            self.pending_reload = None;
            self.set_banner(
                BannerKind::Success,
                if self.tray_autostart {
                    language.text(
                        "Tray autostart enabled.",
                        "Автозапуск приложения в трее включён.",
                    )
                } else {
                    language.text(
                        "Tray autostart disabled.",
                        "Автозапуск приложения в трее выключен.",
                    )
                },
            );
            return;
        }

        let baseline_status = read_status(&self.paths.status)
            .filter(|status| status.schema_version == STATUS_SCHEMA_VERSION);
        let baseline = baseline_status
            .as_ref()
            .map(|status| (status.service_pid, status.config_reload_sequence));
        let not_before_unix_ms = unix_time_ms();
        let reload_wait_ms = config_reload_wait_ms(
            self.persisted.sample_interval_ms,
            self.config.sample_interval_ms,
        );
        match save_config_atomic(&self.paths.config, &self.config) {
            Ok(validated) => {
                self.config.clone_from(&validated);
                self.persisted = validated;
                self.persisted_tray_autostart = self.tray_autostart;
                self.confirmation = Confirmation::None;
                self.service_reload_state = ServiceReloadState::InSync;
                self.set_banner(
                    BannerKind::Information,
                    if autostart_changed {
                        language.text(
                            "Configuration and tray autostart saved. Waiting for the service to reload the configuration...",
                            "Конфигурация и автозапуск приложения в трее сохранены. Ожидание загрузки конфигурации службой...",
                        )
                    } else {
                        language.text(
                            "Configuration saved atomically. Waiting for the service to reload it...",
                            "Конфигурация сохранена атомарно. Ожидание её загрузки службой...",
                        )
                    },
                );
                self.pending_reload = Some(PendingReload {
                    baseline,
                    not_before_unix_ms,
                    expected_mode: self.config.controller_mode,
                    expected_config_fingerprint: self.config.fingerprint(),
                    expected_logging: self.config.logging,
                    deadline: Instant::now() + Duration::from_millis(reload_wait_ms),
                    next_poll: Instant::now(),
                });
            }
            Err(error) => self.report_config_save_failure(&error, autostart_changed),
        }
    }

    fn poll_reload(&mut self, context: &egui::Context) {
        let language = self.language;
        let Some(pending) = self.pending_reload.as_mut() else {
            return;
        };
        match pending.poll(&self.paths, Instant::now()) {
            ReloadPollOutcome::Pending(delay) => context.request_repaint_after(delay),
            ReloadPollOutcome::Reloaded => {
                self.pending_reload = None;
                self.service_reload_state = ServiceReloadState::InSync;
                self.set_banner(
                    BannerKind::Success,
                    language.text(
                        "Configuration applied and reloaded by the WinSched service.",
                        "Конфигурация применена и загружена службой WinSched.",
                    ),
                );
            }
            ReloadPollOutcome::Rejected(detail) => {
                self.pending_reload = None;
                self.service_reload_state = ServiceReloadState::RetryRequired;
                let detail = detail.unwrap_or_else(|| {
                    language
                        .text(
                            "the service did not provide an error",
                            "служба не сообщила подробности ошибки",
                        )
                        .to_owned()
                });
                self.set_banner(
                    BannerKind::Error,
                    format!(
                        "{}: {detail}",
                        language.text(
                            "Service rejected the configuration",
                            "Служба отклонила конфигурацию"
                        )
                    ),
                );
            }
            ReloadPollOutcome::TimedOutWithoutReceipt => {
                self.pending_reload = None;
                self.service_reload_state = ServiceReloadState::RetryRequired;
                self.set_banner(
                    BannerKind::Information,
                    language.text(
                        "Configuration was saved, but the service did not publish a newer reload receipt within the expected interval. Check that the WinSched service is running.",
                        "Конфигурация сохранена, но служба не опубликовала новое подтверждение загрузки за ожидаемое время. Проверьте, что служба WinSched запущена.",
                    ),
                );
            }
            ReloadPollOutcome::TimedOutAfterMismatchedReceipt => {
                self.pending_reload = None;
                self.service_reload_state = ServiceReloadState::RetryRequired;
                self.set_banner(
                    BannerKind::Information,
                    language.text(
                        "The service published a newer reload receipt, but it did not match the complete saved configuration.",
                        "Служба опубликовала новое подтверждение загрузки, но оно не соответствует всей сохранённой конфигурации.",
                    ),
                );
            }
        }
    }

    fn start_diagnostic(&mut self) {
        if self.pending_diagnostic.is_some() {
            return;
        }
        let language = self.language;
        let cancellation = Arc::new(AtomicBool::new(false));
        let thread_cancellation = Arc::clone(&cancellation);
        let (sender, receiver) = mpsc::channel();
        match std::thread::Builder::new()
            .name("winsched-settings-diagnostic".to_owned())
            .spawn(move || {
                let result = diagnostics::run_cancellable(
                    DiagnosticOptions::default(),
                    &thread_cancellation,
                )
                .map_err(|error| error.to_string());
                let _ = sender.send(result);
            }) {
            Ok(_) => {
                self.diagnostic_error = None;
                self.pending_diagnostic = Some(PendingDiagnostic {
                    receiver,
                    cancellation,
                    started: Instant::now(),
                    cancelling: false,
                });
                self.set_banner(
                    BannerKind::Information,
                    language.text(
                        "Passive responsiveness diagnostic started.",
                        "Пассивная диагностика отзывчивости запущена.",
                    ),
                );
            }
            Err(error) => {
                self.diagnostic_error = Some(error.to_string());
                self.set_banner(
                    BannerKind::Error,
                    format!(
                        "{}: {error}",
                        language.text(
                            "Could not start diagnostic worker",
                            "Не удалось запустить поток диагностики"
                        )
                    ),
                );
            }
        }
    }

    fn cancel_diagnostic(&mut self) {
        if let Some(pending) = self.pending_diagnostic.as_mut() {
            pending.cancelling = true;
            pending.cancellation.store(true, Ordering::Relaxed);
        }
    }

    fn poll_diagnostic(&mut self, context: &egui::Context) {
        let Some(pending) = self.pending_diagnostic.as_ref() else {
            return;
        };
        match pending.receiver.try_recv() {
            Ok(Ok(report)) => {
                self.pending_diagnostic = None;
                self.diagnostic_report = Some(report);
                self.diagnostic_error = None;
                self.set_banner(
                    BannerKind::Success,
                    self.language.text(
                        "Passive diagnostic completed.",
                        "Пассивная диагностика завершена.",
                    ),
                );
            }
            Ok(Err(error)) => {
                let cancelled = error == "diagnostic cancelled";
                self.pending_diagnostic = None;
                if cancelled {
                    self.set_banner(
                        BannerKind::Information,
                        self.language
                            .text("Diagnostic cancelled.", "Диагностика отменена."),
                    );
                } else {
                    self.diagnostic_error = Some(error.clone());
                    self.set_banner(
                        BannerKind::Error,
                        format!(
                            "{}: {error}",
                            self.language
                                .text("Diagnostic failed", "Ошибка диагностики")
                        ),
                    );
                }
            }
            Err(mpsc::TryRecvError::Empty) => {
                context.request_repaint_after(Duration::from_millis(100));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending_diagnostic = None;
                let error = self.language.text(
                    "Diagnostic worker exited without a result.",
                    "Поток диагностики завершился без результата.",
                );
                self.diagnostic_error = Some(error.to_owned());
                self.set_banner(BannerKind::Error, error);
            }
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.horizontal_wrapped(|ui| {
            ui.heading(language.text("WinSched Settings", "Настройки WinSched"));
            ui.separator();
            let state = if self.is_dirty() {
                RichText::new(language.text("Unsaved changes", "Есть несохранённые изменения"))
                    .color(Color32::from_rgb(210, 145, 30))
            } else if self.service_reload_state == ServiceReloadState::RetryRequired {
                RichText::new(language.text(
                    "Saved; Apply to retry service",
                    "Сохранено; нажмите «Применить» для повтора",
                ))
                .color(Color32::from_rgb(210, 145, 30))
            } else {
                RichText::new(language.text("Saved", "Сохранено"))
                    .color(Color32::from_rgb(45, 160, 85))
            };
            ui.label(state);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.selectable_value(&mut self.language, Language::Russian, "РУ");
                ui.selectable_value(&mut self.language, Language::English, "EN");
                ui.label(language.text("Language", "Язык"));
            });
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(
                &mut self.tab,
                SettingsTab::General,
                language.text("General", "Основные"),
            );
            ui.selectable_value(
                &mut self.tab,
                SettingsTab::Adaptive,
                language.text("Adaptive", "Адаптивный режим"),
            );
            ui.selectable_value(
                &mut self.tab,
                SettingsTab::Responsiveness,
                language.text("Responsiveness", "Отзывчивость"),
            );
            ui.selectable_value(
                &mut self.tab,
                SettingsTab::BackgroundEfficiency,
                language.text("Background", "Фоновые задачи"),
            );
            ui.selectable_value(
                &mut self.tab,
                SettingsTab::Rules,
                language.text("Process rules", "Правила процессов"),
            );
            ui.selectable_value(
                &mut self.tab,
                SettingsTab::Logging,
                language.text("Logging", "Журнал"),
            );
            ui.selectable_value(
                &mut self.tab,
                SettingsTab::Diagnostics,
                language.text("Diagnostics", "Диагностика"),
            );
        });
        ui.separator();
    }

    fn banner(&self, ui: &mut egui::Ui) {
        let Some(banner) = &self.banner else {
            return;
        };
        let (fill, label) = match banner.kind {
            BannerKind::Information => (
                Color32::from_rgb(42, 67, 93),
                self.language.text("Information", "Информация"),
            ),
            BannerKind::Success => (
                Color32::from_rgb(35, 91, 61),
                self.language.text("Success", "Готово"),
            ),
            BannerKind::Error => (
                Color32::from_rgb(125, 45, 45),
                self.language.text("Error", "Ошибка"),
            ),
        };
        egui::Frame::new()
            .fill(fill)
            .corner_radius(5.0)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.label(RichText::new(label).strong().color(Color32::WHITE));
                ui.label(RichText::new(&banner.message).color(Color32::WHITE));
            });
        ui.add_space(8.0);
    }

    fn general_tab(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.heading(language.text("Controller behavior", "Режим контроллера"));
        ui.label(language.text(
            "Choose whether WinSched observes decisions, applies them, or stays off.",
            "Выберите, будет ли WinSched только наблюдать, применять решения или останется выключенным.",
        ));
        ui.add_space(6.0);
        ui.group(|ui| {
            let controller_help = language.text(
                "Off stops evaluation, Observe reports decisions without changing CPU Sets, and Auto applies validated placement decisions.",
                "«Выключен» останавливает оценку, «Наблюдение» показывает решения без изменения CPU Sets, а Auto применяет проверенные решения размещения.",
            );
            ui.label(RichText::new(language.text("Controller mode", "Режим контроллера")).strong())
                .on_hover_text(controller_help);
            ui.radio_value(
                &mut self.config.controller_mode,
                ControllerMode::Off,
                language.text(
                    "Off — do not observe or change process placement",
                    "Выключен — не наблюдать и не изменять размещение процессов",
                ),
            )
            .on_hover_text(controller_help);
            ui.radio_value(
                &mut self.config.controller_mode,
                ControllerMode::Observe,
                language.text(
                    "Observe — calculate and report decisions without applying them",
                    "Наблюдение — рассчитывать и показывать решения без их применения",
                ),
            )
            .on_hover_text(controller_help);
            ui.radio_value(
                &mut self.config.controller_mode,
                ControllerMode::Auto,
                language.text(
                    "Auto — apply validated CPU placement decisions",
                    "Авто — применять проверенные решения по размещению на CPU",
                ),
            )
            .on_hover_text(controller_help);
        });
        ui.add_space(10.0);
        general_values(ui, &mut self.config, language);
        ui.add_space(8.0);
        let all_processes_help = language.text(
            "When enabled, eligible interactive user processes may be managed even without an explicit rule. Safety exclusions and the utilization threshold still apply.",
            "Если включено, подходящие интерактивные процессы пользователя могут управляться без явного правила. Защитные исключения и порог загрузки продолжают действовать.",
        );
        ui.checkbox(
            &mut self.config.all_user_processes,
            language.text(
                "Manage all eligible user processes, not only explicitly listed rules",
                "Управлять всеми подходящими пользовательскими процессами, а не только указанными в правилах",
            ),
        )
        .on_hover_text(all_processes_help);
        ui.label(language.text(
            "When disabled, a process must have an exact executable-name rule before WinSched considers it.",
            "Если флажок снят, WinSched рассматривает процесс только при наличии правила с точным именем исполняемого файла.",
        ))
        .on_hover_text(all_processes_help);
        ui.add_space(10.0);
        let autostart_help = language.text(
            "Controls the machine-wide Startup shortcut. It starts the non-elevated tray after interactive sign-in; the service starts independently with Windows.",
            "Управляет общесистемным ярлыком автозагрузки. Он запускает трей без повышения прав после интерактивного входа; служба запускается с Windows независимо.",
        );
        ui.checkbox(
            &mut self.tray_autostart,
            language.text(
                "Start the WinSched tray automatically when a user signs in",
                "Автоматически запускать WinSched в области уведомлений при входе пользователя",
            ),
        )
        .on_hover_text(autostart_help);
        ui.label(language.text(
            "This manages the machine-wide WinSched Tray shortcut in the Windows Startup folder.",
            "Этот параметр управляет общесистемным ярлыком WinSched Tray в папке автозагрузки Windows.",
        ))
        .on_hover_text(autostart_help);
    }

    fn adaptive_tab(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.heading(language.text(
            "Adaptive placement policy",
            "Политика адаптивного размещения",
        ));
        ui.label(language.text(
            "These safeguards control when an overloaded process may move between last-level cache domains.",
            "Эти ограничения определяют, когда загруженный процесс можно перемещать между доменами кеша последнего уровня.",
        ));
        ui.add_space(8.0);
        policy_value_u16(
            ui,
            language.text(
                "Overload threshold (basis points)",
                "Порог перегрузки (базисные пункты)",
            ),
            &mut self.config.policy.overload_threshold_bps,
            0..=10_000,
            language.text(
                "A domain is considered overloaded at or above this utilization.",
                "Домен считается перегруженным при достижении этой загрузки.",
            ),
        );
        policy_value_u16(
            ui,
            language.text(
                "Minimum improvement (basis points)",
                "Минимальное улучшение (базисные пункты)",
            ),
            &mut self.config.policy.minimum_improvement_bps,
            0..=10_000,
            language.text(
                "Required load advantage before moving to another domain.",
                "Необходимая разница в загрузке перед перемещением в другой домен.",
            ),
        );
        policy_value_u16(
            ui,
            language.text("Stability samples", "Стабильные измерения"),
            &mut self.config.policy.stability_samples,
            1..=u16::MAX,
            language.text(
                "Consecutive overloaded samples required before a move.",
                "Число последовательных измерений перегрузки перед перемещением.",
            ),
        );
        policy_value_u64(
            ui,
            language.text(
                "Minimum residency (milliseconds)",
                "Минимальное время размещения (миллисекунды)",
            ),
            &mut self.config.policy.minimum_residency_ms,
            language.text(
                "Minimum time a process remains in its assigned domain.",
                "Минимальное время нахождения процесса в назначенном домене.",
            ),
        );
        policy_value_u64(
            ui,
            language.text(
                "Cooldown (milliseconds)",
                "Пауза между перемещениями (миллисекунды)",
            ),
            &mut self.config.policy.cooldown_ms,
            language.text(
                "Minimum delay between moves of the same process.",
                "Минимальная задержка между перемещениями одного процесса.",
            ),
        );
        policy_value_u16(
            ui,
            language.text(
                "Maximum mutations per evaluation",
                "Максимум изменений за одну оценку",
            ),
            &mut self.config.policy.max_mutations_per_evaluation,
            1..=u16::MAX,
            language.text(
                "Rate limit for placement changes during one evaluation.",
                "Ограничение числа изменений размещения за одну оценку.",
            ),
        );
    }

    #[allow(clippy::too_many_lines)] // One tab keeps the three bilingual log levels together.
    fn logging_tab(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.heading(language.text("Diagnostic logging", "Диагностический журнал"));
        ui.label(language.text(
            "Control how much service diagnostic history is kept on this computer.",
            "Настройте объём истории диагностики службы, сохраняемой на этом компьютере.",
        ));
        ui.add_space(8.0);
        let logging_help = language.text(
            "Off performs no routine log writes. Normal records changes, failures, and one aggregated decision summary per minute. Trace additionally writes every per-process policy decision and can generate substantial disk I/O.",
            "Off отключает обычные записи. Normal записывает изменения, ошибки и одну агрегированную сводку решений в минуту. Trace дополнительно записывает каждое решение по процессу и может создавать значительный дисковый I/O.",
        );
        ui.label(language.text("Log detail level", "Уровень детализации журнала"))
            .on_hover_text(logging_help);
        ui.horizontal(|ui| {
            ui.radio_value(
                &mut self.config.logging.level,
                LoggingLevel::Off,
                language.text("Off", "Выключен"),
            )
            .on_hover_text(logging_help);
            ui.radio_value(
                &mut self.config.logging.level,
                LoggingLevel::Normal,
                language.text("Normal (recommended)", "Обычный (рекомендуется)"),
            )
            .on_hover_text(logging_help);
            ui.radio_value(
                &mut self.config.logging.level,
                LoggingLevel::Trace,
                language.text("Trace", "Трассировка"),
            )
            .on_hover_text(logging_help);
        });
        ui.label(language.text(
            "Diagnostic events are written as JSON lines to:",
            "Диагностические события записываются строками JSON в:",
        ))
        .on_hover_text(logging_help);
        ui.monospace(self.paths.log.display().to_string());
        ui.add_space(8.0);

        match self.config.logging.level {
            LoggingLevel::Off => {
                ui.group(|ui| {
                    ui.label(language.text(
                        "Routine logging is off. Existing log and archive files are preserved.",
                        "Обычный журнал выключен. Существующие файлы журнала и архивы сохраняются.",
                    ));
                });
                ui.add_space(8.0);
            }
            LoggingLevel::Normal => {
                ui.group(|ui| {
                    ui.label(language.text(
                        "Normal records important events immediately and coalesces unchanged process decisions into a one-minute summary.",
                        "Normal немедленно записывает важные события и объединяет неизменившиеся решения по процессам в минутную сводку.",
                    ));
                });
                ui.add_space(8.0);
            }
            LoggingLevel::Trace => {
                ui.group(|ui| {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        language.text(
                            "Trace is intended only for short diagnostics and may rotate the log frequently.",
                            "Trace предназначен только для короткой диагностики и может часто ротировать журнал.",
                        ),
                    );
                });
                ui.add_space(8.0);
            }
        }

        let logging_enabled = self.config.logging.level != LoggingLevel::Off;
        logging_value_u16(
            ui,
            language.text(
                "Maximum active log size (MiB)",
                "Максимальный размер активного журнала (МиБ)",
            ),
            &mut self.config.logging.max_file_size_mib,
            logging_enabled,
            language.text(
                "The active log is rotated before a new diagnostic record would exceed this size.",
                "Активный журнал ротируется до записи диагностического события, которое превысило бы этот размер.",
            ),
            language.text(" MiB", " МиБ"),
        );
        logging_value_u8(
            ui,
            language.text(
                "Retained circular archives",
                "Сохраняемые циклические архивы",
            ),
            &mut self.config.logging.retained_archives,
            logging_enabled,
            language.text(
                "0 reuses the active file without archives. Otherwise winsched.log.1 is newest and the oldest retained archive is removed.",
                "0 означает повторное использование активного файла без архивов. В остальных случаях winsched.log.1 — самый новый архив, а самый старый сохраняемый архив удаляется.",
            ),
        );

        let file_count = u64::from(self.config.logging.retained_archives) + 1;
        let estimated_mib = u64::from(self.config.logging.max_file_size_mib) * file_count;
        ui.label(match language {
            Language::English => format!(
                "Estimated maximum log storage: {estimated_mib} MiB ({} MiB per file, {file_count} files).",
                self.config.logging.max_file_size_mib
            ),
            Language::Russian => format!(
                "Расчётный максимальный объём журнала: {estimated_mib} МиБ (размер файла {} МиБ, файлов: {file_count}).",
                self.config.logging.max_file_size_mib
            ),
        });
        ui.add_space(8.0);
        ui.label(language.text(
            "Critical startup failures may still be written to the separate winsched-emergency.log file.",
            "Критические ошибки запуска могут по-прежнему записываться в отдельный файл winsched-emergency.log.",
        ));
    }

    fn diagnostics_tab(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.heading(language.text("Passive diagnostics", "Пассивная диагностика"));
        ui.label(language.text(
            "Measures scheduler, CPU, memory, taskbar, Explorer, WSL, and VMware signals for 10 seconds. It never clicks, moves the pointer, changes focus, edits .wslconfig, or changes CPU Sets.",
            "В течение 10 секунд измеряет планировщик, CPU, память, панель задач, Explorer, WSL и VMware. Диагностика не нажимает кнопки, не двигает указатель, не меняет фокус, не редактирует .wslconfig и не изменяет CPU Sets.",
        ));
        ui.add_space(8.0);
        self.controller_efficiency_status(ui);
        ui.add_space(12.0);

        if let Some(pending) = &self.pending_diagnostic {
            let elapsed = pending.started.elapsed().as_secs_f32();
            ui.label(if pending.cancelling {
                language.text("Cancelling...", "Отмена...").to_owned()
            } else {
                match language {
                    Language::English => format!("Collecting passive samples... {elapsed:.1} s"),
                    Language::Russian => format!("Сбор пассивных измерений... {elapsed:.1} с"),
                }
            });
            if ui
                .add_enabled(
                    !pending.cancelling,
                    egui::Button::new(language.text("Cancel", "Отменить")),
                )
                .on_hover_text(language.text(
                    "Stops after the current bounded sample.",
                    "Останавливает диагностику после текущего ограниченного измерения.",
                ))
                .clicked()
            {
                self.cancel_diagnostic();
            }
        } else if ui
            .button(language.text(
                "Run passive 10-second diagnostic",
                "Запустить пассивную диагностику на 10 секунд",
            ))
            .on_hover_text(language.text(
                "Runs in a background worker so the Settings window remains responsive.",
                "Выполняется в фоновом потоке, поэтому окно настроек остаётся отзывчивым.",
            ))
            .clicked()
        {
            self.start_diagnostic();
        }

        if let Some(error) = &self.diagnostic_error {
            ui.colored_label(Color32::from_rgb(210, 70, 70), error);
        }
        let Some(report) = self.diagnostic_report.clone() else {
            return;
        };
        ui.add_space(12.0);
        diagnostic_metrics(ui, &report, language);
        ui.add_space(10.0);
        ui.heading(language.text("Findings", "Выводы"));
        for finding in &report.findings {
            let (summary, recommendation) = diagnostic_finding_text(finding.code, language);
            let color = match finding.severity {
                DiagnosticSeverity::Information => Color32::from_rgb(90, 150, 210),
                DiagnosticSeverity::Warning => Color32::from_rgb(220, 160, 55),
                DiagnosticSeverity::Critical => Color32::from_rgb(220, 70, 70),
            };
            ui.group(|ui| {
                ui.colored_label(color, format!("{:?}", finding.code));
                ui.label(summary);
                ui.label(recommendation);
            });
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let json = serde_json::to_string_pretty(&report).unwrap_or_default();
            if ui
                .button(language.text("Copy JSON", "Копировать JSON"))
                .on_hover_text(language.text(
                    "Copies the privacy-safe report without window titles or user paths.",
                    "Копирует безопасный отчёт без заголовков окон и пользовательских путей.",
                ))
                .clicked()
            {
                ui.ctx().copy_text(json.clone());
            }
            if ui
                .button(language.text("Save JSON to Downloads", "Сохранить JSON в Загрузки"))
                .on_hover_text(language.text(
                    "Writes one report only after this explicit action.",
                    "Записывает один отчёт только после этого явного действия.",
                ))
                .clicked()
            {
                self.save_diagnostic_report(&json, report.captured_at_unix_ms);
            }
        });
    }

    fn save_diagnostic_report(&mut self, json: &str, captured_at_unix_ms: u64) {
        let language = self.language;
        let Some(profile) = std::env::var_os("USERPROFILE").map(PathBuf::from) else {
            self.set_banner(
                BannerKind::Error,
                language.text(
                    "Cannot find the user profile for report output.",
                    "Не удалось определить профиль пользователя для сохранения отчёта.",
                ),
            );
            return;
        };
        let downloads = profile.join("Downloads");
        let directory = if downloads.is_dir() {
            downloads
        } else {
            profile
        };
        let path = directory.join(format!("WinSched-diagnostic-{captured_at_unix_ms}.json"));
        match fs::write(&path, format!("{json}\n")) {
            Ok(()) => self.set_banner(
                BannerKind::Success,
                format!(
                    "{} {}",
                    language.text("Diagnostic report saved to", "Отчёт диагностики сохранён в"),
                    path.display()
                ),
            ),
            Err(error) => self.set_banner(
                BannerKind::Error,
                format!(
                    "{}: {error}",
                    language.text(
                        "Could not save diagnostic report",
                        "Не удалось сохранить отчёт диагностики"
                    )
                ),
            ),
        }
    }

    #[allow(clippy::too_many_lines)] // One read-only panel keeps related telemetry understandable.
    fn controller_efficiency_status(&self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.heading(language.text("Controller efficiency", "Эффективность контроллера"));
        let Some(status) = read_status(&self.paths.status)
            .filter(|status| status.schema_version == STATUS_SCHEMA_VERSION)
        else {
            ui.label(match language {
                Language::English => format!(
                    "Self-observability is available when a status-schema-{STATUS_SCHEMA_VERSION} service is running."
                ),
                Language::Russian => format!(
                    "Самодиагностика доступна после запуска службы со схемой статуса {STATUS_SCHEMA_VERSION}."
                ),
            });
            return;
        };
        let Some(telemetry) = status.telemetry else {
            ui.label(language.text(
                "The running service has not published self-observability yet.",
                "Запущенная служба ещё не опубликовала самодиагностику.",
            ));
            return;
        };

        let evaluation = telemetry.evaluation;
        ui.label(match language {
            Language::English => format!(
                "Evaluations: {} total, {} in the rolling window. Last/mean/p95/max: {}/{}/{}/{} us.",
                evaluation.completed_total,
                evaluation.window_samples,
                evaluation.last_duration_us,
                evaluation.rolling_mean_us,
                evaluation.rolling_p95_us,
                evaluation.rolling_max_us,
            ),
            Language::Russian => format!(
                "Оценки: всего {}, в скользящем окне {}. Последняя/средняя/p95/максимум: {}/{}/{}/{} мкс.",
                evaluation.completed_total,
                evaluation.window_samples,
                evaluation.last_duration_us,
                evaluation.rolling_mean_us,
                evaluation.rolling_p95_us,
                evaluation.rolling_max_us,
            ),
        });
        ui.label(match language {
            Language::English => format!(
                "Last pass: {} scanned, {} eligible, {} decisions, {} currently managed.",
                evaluation.last_scanned_processes,
                evaluation.last_eligible_processes,
                evaluation.last_decisions,
                status.managed_processes,
            ),
            Language::Russian => format!(
                "Последний проход: просмотрено {}, подходят {}, решений {}, сейчас управляются {}.",
                evaluation.last_scanned_processes,
                evaluation.last_eligible_processes,
                evaluation.last_decisions,
                status.managed_processes,
            ),
        });

        let logging = telemetry.logging;
        ui.label(match language {
            Language::English => format!(
                "File log since service start: {} records / {} KiB, {} write errors. Status writes: {}.",
                logging.records_written,
                logging.bytes_written / 1024,
                logging.write_errors,
                logging.status_writes,
            ),
            Language::Russian => format!(
                "Файловый журнал с запуска службы: {} записей / {} КиБ, ошибок записи: {}. Записей status: {}.",
                logging.records_written,
                logging.bytes_written / 1024,
                logging.write_errors,
                logging.status_writes,
            ),
        });

        let mutations = telemetry.mutations;
        ui.label(match language {
            Language::English => format!(
                "Mutations: placement {}/{}/{} and Background {}/{}/{} attempted/succeeded/failed.",
                mutations.placement_attempted,
                mutations.placement_succeeded,
                mutations.placement_failed,
                mutations.background_attempted,
                mutations.background_succeeded,
                mutations.background_failed,
            ),
            Language::Russian => format!(
                "Изменения: размещение {}/{}/{} и Background {}/{}/{} попыток/успешно/ошибок.",
                mutations.placement_attempted,
                mutations.placement_succeeded,
                mutations.placement_failed,
                mutations.background_attempted,
                mutations.background_succeeded,
                mutations.background_failed,
            ),
        });

        if let Some(process) = telemetry.service_process {
            let one_core_bps = process
                .cpu_time_100ns
                .checked_div(process.uptime_ms)
                .unwrap_or(0);
            ui.label(match language {
                Language::English => format!(
                    "Service process: {}.{:02}% of one core average, {} MiB working set, uptime {} s.",
                    one_core_bps / 100,
                    one_core_bps % 100,
                    process.working_set_bytes.div_ceil(1024 * 1024),
                    process.uptime_ms / 1000,
                ),
                Language::Russian => format!(
                    "Процесс службы: в среднем {}.{:02}% одного ядра, working set {} МиБ, uptime {} с.",
                    one_core_bps / 100,
                    one_core_bps % 100,
                    process.working_set_bytes.div_ceil(1024 * 1024),
                    process.uptime_ms / 1000,
                ),
            });
        } else {
            ui.label(language.text(
                "Service process resource counters are unavailable.",
                "Счётчики ресурсов процесса службы недоступны.",
            ));
        }
    }

    fn responsiveness_tab(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.heading(language.text(
            "System responsiveness reserve",
            "Системный резерв отзывчивости",
        ));
        ui.label(language.text(
            "Keep whole physical cores available to Windows by excluding their CPU Sets only from managed application assignments.",
            "Оставляет целые физические ядра доступными Windows, исключая их CPU Sets только из назначений управляемых приложений.",
        ));
        ui.add_space(8.0);
        self.responsiveness_reserve_controls(ui);
        ui.add_space(10.0);
        self.responsiveness_memory_controls(ui);
        ui.add_space(10.0);
        self.responsiveness_live_status(ui);
    }

    fn responsiveness_reserve_controls(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        let reserve_help = reserve_help(language);
        ui.checkbox(
            &mut self.config.responsiveness.enabled,
            language.text(
                "Enable topology-aware system reserve",
                "Включить топологический системный резерв",
            ),
        )
        .on_hover_text(reserve_help);
        ui.label(language.text(
            "Protected system processes remain unrestricted and are never pinned to the reserve. The reserve is spread over LLC domains and always includes complete SMT sibling pairs.",
            "Защищённые системные процессы остаются без ограничений и не закрепляются за резервом. Резерв распределяется по LLC и всегда включает полные пары SMT-потоков.",
        ))
        .on_hover_text(reserve_help);
        ui.add_space(8.0);
        let controls_enabled = self.config.responsiveness.enabled;

        responsiveness_value_u8(
            ui,
            language.text("System reserve percent", "Процент системного резерва"),
            &mut self.config.responsiveness.system_reserve_percent,
            MIN_SYSTEM_RESERVE_PERCENT..=MAX_SYSTEM_RESERVE_PERCENT,
            controls_enabled,
            language.text(
                "The percentage is calculated from physical cores and rounded upward.",
                "Процент рассчитывается от физических ядер и округляется вверх.",
            ),
            "%",
        );
        responsiveness_value_u16(
            ui,
            language.text("Minimum reserved cores", "Минимум резервных ядер"),
            &mut self.config.responsiveness.minimum_reserved_cores,
            1..=MAX_CONFIGURED_PHYSICAL_CORES,
            controls_enabled,
            language.text(
                "Lower bound for small and medium processors.",
                "Нижняя граница для процессоров с небольшим и средним числом ядер.",
            ),
        );
        responsiveness_value_u16(
            ui,
            language.text("Maximum reserved cores", "Максимум резервных ядер"),
            &mut self.config.responsiveness.maximum_reserved_cores,
            1..=MAX_CONFIGURED_PHYSICAL_CORES,
            controls_enabled,
            language.text(
                "Upper bound that prevents a percentage from consuming too much capacity.",
                "Верхняя граница, не позволяющая проценту занять слишком большую часть CPU.",
            ),
        );
        latency_guard_toggle(
            ui,
            &mut self.config.responsiveness.latency_guard_enabled,
            controls_enabled,
            language,
        );
        responsiveness_value_u64(
            ui,
            language.text(
                "Latency target p99 (microseconds)",
                "Целевая задержка p99 (мкс)",
            ),
            &mut self.config.responsiveness.latency_target_p99_us,
            MIN_LATENCY_THRESHOLD_US..=MAX_LATENCY_THRESHOLD_US,
            controls_enabled,
            language.text(
                "Sustained values above this threshold shrink memory-profile concurrency.",
                "Устойчивые значения выше этого порога уменьшают параллелизм memory-профиля.",
            ),
        );
        responsiveness_value_u64(
            ui,
            language.text(
                "Latency recovery p99 (microseconds)",
                "Порог восстановления p99 (мкс)",
            ),
            &mut self.config.responsiveness.latency_recovery_p99_us,
            MIN_LATENCY_THRESHOLD_US..=MAX_LATENCY_THRESHOLD_US,
            controls_enabled,
            language.text(
                "Sustained values at or below this threshold restore one physical core after cooldown.",
                "Устойчивые значения не выше этого порога возвращают одно физическое ядро после cooldown.",
            ),
        );
        responsiveness_value_u16(
            ui,
            language.text(
                "Adjustment stability samples",
                "Стабильные измерения перед изменением",
            ),
            &mut self.config.responsiveness.adjustment_stability_samples,
            1..=MAX_RESPONSIVENESS_STABILITY_SAMPLES,
            controls_enabled,
            language.text(
                "Consecutive service evaluations required before changing memory width.",
                "Число последовательных оценок службы перед изменением ширины memory-профиля.",
            ),
        );
    }

    fn responsiveness_memory_controls(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        let controls_enabled = self.config.responsiveness.enabled;
        ui.heading(language.text(
            "Memory-bound workload profile",
            "Профиль нагрузок, ограниченных памятью",
        ));
        let smt_help = language.text(
            "Off keeps one logical processor per physical core, which is recommended for bandwidth-bound workloads. On permits both SMT siblings and can help compute-heavy mixed workloads.",
            "В выключенном состоянии остаётся один логический процессор на физическое ядро, что рекомендуется для нагрузок, ограниченных памятью. Включение разрешает оба SMT-потока и может помочь смешанным вычислительным нагрузкам.",
        );
        ui.add_enabled(
            controls_enabled,
            egui::Checkbox::new(
                &mut self.config.responsiveness.memory.use_smt,
                language.text(
                    "Allow both SMT threads per physical core",
                    "Разрешить оба SMT-потока физического ядра",
                ),
            ),
        )
        .on_hover_text(smt_help);
        ui.label(language.text(
            "Off is recommended for bandwidth-bound workloads on this Threadripper; the profile keeps one logical processor per physical core.",
            "Для ограниченных памятью нагрузок на этом Threadripper рекомендуется выключить: профиль оставляет один логический процессор на физическое ядро.",
        ))
        .on_hover_text(smt_help);
        responsiveness_value_u16(
            ui,
            language.text(
                "Minimum memory-profile cores",
                "Минимум ядер memory-профиля",
            ),
            &mut self.config.responsiveness.memory.minimum_physical_cores,
            1..=MAX_CONFIGURED_PHYSICAL_CORES,
            controls_enabled,
            language.text(
                "The adaptive width will never shrink below this value.",
                "Адаптивная ширина никогда не уменьшится ниже этого значения.",
            ),
        );
        responsiveness_value_u16(
            ui,
            language.text(
                "Maximum memory-profile cores",
                "Максимум ядер memory-профиля",
            ),
            &mut self.config.responsiveness.memory.maximum_physical_cores,
            1..=MAX_CONFIGURED_PHYSICAL_CORES,
            controls_enabled,
            language.text(
                "The adaptive width will never grow beyond this value.",
                "Адаптивная ширина никогда не вырастет выше этого значения.",
            ),
        );
        responsiveness_value_u64(
            ui,
            language.text(
                "Memory resize cooldown (milliseconds)",
                "Пауза изменения ширины memory-профиля (миллисекунды)",
            ),
            &mut self.config.responsiveness.memory.resize_cooldown_ms,
            MIN_MEMORY_RESIZE_COOLDOWN_MS..=MAX_MEMORY_RESIZE_COOLDOWN_MS,
            controls_enabled,
            language.text(
                "Prevents rapid concurrency oscillation and cache churn.",
                "Предотвращает частые изменения параллелизма и вытеснение кеша.",
            ),
        );
    }

    #[allow(clippy::too_many_lines)] // One panel keeps the complete live reserve state together.
    fn responsiveness_live_status(&self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.heading(language.text("Live service plan", "Текущий план службы"));
        match read_status(&self.paths.status)
            .filter(|status| status.schema_version == STATUS_SCHEMA_VERSION)
        {
            Some(status) => {
                let reserve = status.system_reserve;
                ui.label(match language {
                    Language::English => format!(
                        "Physical cores: {}. Reserved: {} cores / {} CPU Sets across {} LLC domains.",
                        reserve.physical_core_count,
                        reserve.reserved_physical_cores.len(),
                        reserve.reserved_cpu_set_ids.len(),
                        reserve.covered_llc_domains.len(),
                    ),
                    Language::Russian => format!(
                        "Физических ядер: {}. Резерв: {} ядер / {} CPU Sets в {} доменах LLC.",
                        reserve.physical_core_count,
                        reserve.reserved_physical_cores.len(),
                        reserve.reserved_cpu_set_ids.len(),
                        reserve.covered_llc_domains.len(),
                    ),
                });
                if !reserve.reserved_physical_cores.is_empty() {
                    ui.monospace(
                        reserve
                            .reserved_physical_cores
                            .iter()
                            .map(|core| format!("G{}:C{}", core.group, core.core_index))
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                }
                let latency = status.scheduler_latency;
                ui.label(match language {
                    Language::English => format!(
                        "Scheduling latency guard: {}. Window: {} samples; p95 {} us, p99 {} us, maximum {} us.",
                        if latency.enabled { "enabled" } else { "disabled" },
                        latency.window_samples,
                        latency.p95_lateness_us,
                        latency.p99_lateness_us,
                        latency.maximum_lateness_us,
                    ),
                    Language::Russian => format!(
                        "Контроль задержки планирования: {}. Окно: {} измерений; p95 {} мкс, p99 {} мкс, максимум {} мкс.",
                        if latency.enabled { "включён" } else { "выключен" },
                        latency.window_samples,
                        latency.p95_lateness_us,
                        latency.p99_lateness_us,
                        latency.maximum_lateness_us,
                    ),
                });
                ui.label(match language {
                    Language::English => format!(
                        "Maximum LLC telemetry: DPC {}.{:02}%, interrupts {}.{:02}%.",
                        status.maximum_dpc_time_bps / 100,
                        status.maximum_dpc_time_bps % 100,
                        status.maximum_interrupt_time_bps / 100,
                        status.maximum_interrupt_time_bps % 100,
                    ),
                    Language::Russian => format!(
                        "Максимум по LLC: DPC {}.{:02}%, прерывания {}.{:02}%.",
                        status.maximum_dpc_time_bps / 100,
                        status.maximum_dpc_time_bps % 100,
                        status.maximum_interrupt_time_bps / 100,
                        status.maximum_interrupt_time_bps % 100,
                    ),
                });
                let pressure = match (language, status.responsiveness_pressure) {
                    (Language::English, ResponsivenessPressure::Unknown) => "unknown",
                    (Language::English, ResponsivenessPressure::Normal) => "normal",
                    (Language::English, ResponsivenessPressure::Elevated) => "elevated",
                    (Language::Russian, ResponsivenessPressure::Unknown) => "нет данных",
                    (Language::Russian, ResponsivenessPressure::Normal) => "норма",
                    (Language::Russian, ResponsivenessPressure::Elevated) => "повышено",
                };
                ui.label(match language {
                    Language::English => format!(
                        "Memory-profile width: {} physical cores; responsiveness pressure: {pressure}.",
                        status.memory_profile_physical_cores,
                    ),
                    Language::Russian => format!(
                        "Ширина memory-профиля: {} физических ядер; давление на отзывчивость: {pressure}.",
                        status.memory_profile_physical_cores,
                    ),
                });
                if let Some(adjustment) = &status.last_responsiveness_adjustment {
                    ui.label(match language {
                        Language::English => format!("Last adjustment: {adjustment}"),
                        Language::Russian => format!("Последнее изменение: {adjustment}"),
                    });
                }
            }
            None => {
                ui.label(match language {
                    Language::English => format!(
                        "Live reserve information is unavailable until a status-schema-{STATUS_SCHEMA_VERSION} service is running."
                    ),
                    Language::Russian => format!(
                        "Информация о резерве появится после запуска службы со схемой статуса {STATUS_SCHEMA_VERSION}."
                    ),
                });
            }
        }
    }

    #[allow(clippy::too_many_lines)] // One tab keeps related bilingual controls together.
    fn background_efficiency_tab(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.heading(language.text("Background efficiency", "Эффективность фоновых задач"));
        ui.label(language.text(
            "Applies only to an exact process rule whose workload profile is Background. Broad process scope and the default profile never opt a process into this policy.",
            "Применяется только к точному правилу процесса с профилем нагрузки Background. Общий охват процессов и профиль по умолчанию никогда не включают эту политику.",
        ));
        ui.label(language.text(
            "The non-elevated tray supplies foreground, visible-window, and active-audio veto signals. Missing or stale signals request a safe restore on the bounded safety cadence.",
            "Трей без повышения прав передаёт запрещающие сигналы foreground, видимых окон и активного аудио. При отсутствии или устаревании сигналов безопасное восстановление выполняется в пределах ограниченного защитного интервала.",
        ));
        ui.add_space(8.0);

        let master_help = language.text(
            "Enables journaled process-level EcoQoS and memory-priority handling for explicitly marked background processes. Both mutations are off by default: native acceptance confirmed that a parent's memory priority propagates to children created later, and parent rollback does not restore those live children. Enable a property only for a known leaf workload.",
            "Включает журналируемое управление EcoQoS и приоритетом памяти на уровне процесса для явно отмеченных фоновых процессов. Оба изменения по умолчанию выключены: native-тест подтвердил передачу приоритета памяти новым дочерним процессам, а восстановление родителя не восстанавливает уже работающих детей. Включайте параметр только для заведомо конечной фоновой задачи без дочерних процессов.",
        );
        ui.checkbox(
            &mut self.config.background_efficiency.enabled,
            language.text(
                "Enable background efficiency",
                "Включить эффективность фоновых задач",
            ),
        )
        .on_hover_text(master_help);
        let enabled = self.config.background_efficiency.enabled;

        background_efficiency_toggle(
            ui,
            &mut self.config.background_efficiency.eco_qos_enabled,
            enabled,
            language.text("Apply EcoQoS", "Применять EcoQoS"),
            language.text(
                "Opt-in for known leaf workloads. The validated cmd-to-ping case did not inherit EcoQoS, but process-wide behavior must still be tested across the target application's complete child tree. WinSched never forces HighQoS.",
                "Только для проверенных конечных фоновых задач. В тесте cmd→ping EcoQoS не наследовался, но поведение конкретного приложения всё равно нужно проверить по всему дереву дочерних процессов. WinSched никогда не навязывает HighQoS.",
            ),
        );
        background_efficiency_toggle(
            ui,
            &mut self.config.background_efficiency.memory_priority_enabled,
            enabled,
            language.text(
                "Lower background memory priority",
                "Понижать приоритет памяти фоновых задач",
            ),
            language.text(
                "Opt-in for known leaf workloads. Uses Below Normal as the process default for pages added to its working set. Windows propagated this value to a later child in native acceptance; restoring the parent neither restores that child nor immediately retags pages populated meanwhile.",
                "Только для проверенных конечных фоновых задач. Задаёт Below Normal как приоритет процесса по умолчанию для новых страниц working set. Native-тест подтвердил передачу значения новому дочернему процессу; восстановление родителя не восстанавливает ребёнка и не меняет мгновенно метки уже загруженных страниц.",
            ),
        );
        let memory_guard_enabled =
            enabled && self.config.background_efficiency.memory_priority_enabled;
        background_efficiency_toggle(
            ui,
            &mut self
                .config
                .background_efficiency
                .memory_pressure_guard_enabled,
            memory_guard_enabled,
            language.text(
                "React to Windows low-memory notifications",
                "Реагировать на уведомления Windows о нехватке памяти",
            ),
            language.text(
                "Changes owned background memory priority from Below Normal to Low only while Windows reports low-memory pressure, with system hysteresis.",
                "Меняет управляемый приоритет фоновой памяти с Below Normal на Low только пока Windows сообщает о нехватке памяти, с системным гистерезисом.",
            ),
        );

        ui.add_space(8.0);
        ui.label(RichText::new(language.text("Safety guards", "Защитные условия")).strong());
        background_efficiency_toggle(
            ui,
            &mut self.config.background_efficiency.protect_foreground,
            enabled,
            language.text(
                "Protect the foreground application",
                "Защищать приложение foreground",
            ),
            language.text(
                "A foreground process and its matching process cohort are restored immediately.",
                "Процесс foreground и связанные процессы с тем же правилом немедленно восстанавливаются.",
            ),
        );
        background_efficiency_toggle(
            ui,
            &mut self.config.background_efficiency.protect_visible,
            enabled,
            language.text(
                "Protect visible and minimized applications",
                "Защищать видимые и свёрнутые приложения",
            ),
            language.text(
                "Includes minimized top-level windows so restoring Firefox or Explorer from the taskbar is never delayed by this policy.",
                "Включает свёрнутые окна верхнего уровня, чтобы эта политика не задерживала восстановление Firefox или Explorer с панели задач.",
            ),
        );
        background_efficiency_toggle(
            ui,
            &mut self.config.background_efficiency.protect_audio,
            enabled,
            language.text(
                "Protect applications with active audio",
                "Защищать приложения с активным аудио",
            ),
            language.text(
                "Protects active render and capture sessions, including playback, calls, and recording.",
                "Защищает активные сессии воспроизведения и захвата, включая видео, звонки и запись.",
            ),
        );

        ui.add_space(12.0);
        ui.heading(language.text("Live service state", "Текущее состояние службы"));
        match read_status(&self.paths.status)
            .filter(|status| status.schema_version == STATUS_SCHEMA_VERSION)
        {
            Some(status) => {
                let background = status.background_efficiency;
                ui.label(match language {
                    Language::English => format!(
                        "Eligible: {}. Managed: {}. Protected: {}.",
                        background.eligible_processes,
                        background.managed_processes,
                        background.protected_processes,
                    ),
                    Language::Russian => format!(
                        "Подходят: {}. Управляются: {}. Защищены: {}.",
                        background.eligible_processes,
                        background.managed_processes,
                        background.protected_processes,
                    ),
                });
                ui.label(match language {
                    Language::English => format!(
                        "Interactive sensors: {} available / {} required sessions. Memory monitor: {}. Pressure: {}.",
                        background.interactive_probe_sessions,
                        background.required_probe_sessions,
                        if background.memory_pressure_monitor_available { "available" } else { "unavailable" },
                        if background.low_memory_condition { "low memory" } else { "normal" },
                    ),
                    Language::Russian => format!(
                        "Интерактивные датчики: доступно {} / требуется {} сессий. Контроль памяти: {}. Давление: {}.",
                        background.interactive_probe_sessions,
                        background.required_probe_sessions,
                        if background.memory_pressure_monitor_available { "доступен" } else { "недоступен" },
                        if background.low_memory_condition { "нехватка памяти" } else { "норма" },
                    ),
                });
                if let Some(action) = background.last_action {
                    ui.label(match language {
                        Language::English => format!("Last transition: {action}"),
                        Language::Russian => format!("Последний переход: {action}"),
                    });
                }
            }
            None => {
                ui.label(match language {
                    Language::English => format!(
                        "Live background-efficiency information is unavailable until a status-schema-{STATUS_SCHEMA_VERSION} service is running."
                    ),
                    Language::Russian => format!(
                        "Информация об эффективности фоновых задач появится после запуска службы со схемой статуса {STATUS_SCHEMA_VERSION}."
                    ),
                });
            }
        }
    }

    fn rules_tab(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.horizontal(|ui| {
            ui.heading(language.text("Process rules", "Правила процессов"));
            if ui
                .button(language.text("Add process rule", "Добавить правило процесса"))
                .clicked()
            {
                self.config.rules.push(ProcessRule {
                    image: String::new(),
                    mode: RuleMode::Auto,
                    profile: WorkloadProfile::Balanced,
                    group: None,
                    llc: None,
                });
            }
        });
        ui.label(language.text(
            "Rules match an executable file name exactly and case-insensitively. Paths are not allowed.",
            "Правила сопоставляются с точным именем исполняемого файла без учёта регистра. Пути не допускаются.",
        ));
        ui.add_space(8.0);

        if self.config.rules.is_empty() {
            ui.group(|ui| {
                ui.label(language.text(
                    "No explicit process rules are configured.",
                    "Явные правила процессов не настроены.",
                ));
                ui.label(language.text(
                    "Use Add process rule to create one.",
                    "Нажмите «Добавить правило процесса», чтобы создать его.",
                ));
            });
            return;
        }

        let mut remove = None;
        let requested_image = self.rule_focus_image.clone();
        let mut requested_rule_shown = false;
        for (index, rule) in self.config.rules.iter_mut().enumerate() {
            let requested = requested_image
                .as_deref()
                .is_some_and(|image| rule.image.eq_ignore_ascii_case(image));
            if process_rule_ui(ui, index, rule, language, requested) {
                remove = Some(index);
            }
            requested_rule_shown |= requested;
            ui.add_space(8.0);
        }
        if requested_rule_shown {
            self.rule_focus_image = None;
        }
        if let Some(index) = remove {
            self.config.rules.remove(index);
        }
    }

    fn restore_panel(&mut self, ui: &mut egui::Ui) {
        if self.confirmation != Confirmation::RestoreDefaults {
            return;
        }
        let language = self.language;
        egui::Frame::new()
            .fill(Color32::from_rgb(94, 65, 27))
            .corner_radius(5.0)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(language.text(
                        "Restore every setting and remove all process rules?",
                        "Восстановить все настройки и удалить все правила процессов?",
                    ))
                    .strong()
                    .color(Color32::WHITE),
                );
                ui.label(
                    RichText::new(language.text(
                        "Defaults are loaded into the editor first; Apply is still required to save them.",
                        "Сначала значения по умолчанию будут загружены в редактор; для сохранения всё равно потребуется нажать «Применить».",
                    ))
                        .color(Color32::WHITE),
                );
                ui.horizontal(|ui| {
                    if ui
                        .button(language.text(
                            "Confirm restore defaults",
                            "Подтвердить восстановление",
                        ))
                        .clicked()
                    {
                        self.confirmation = Confirmation::None;
                        match restore_defaults() {
                            Ok(defaults) => {
                                self.config = defaults;
                                self.tray_autostart = true;
                                self.set_banner(
                                    BannerKind::Information,
                                    language.text(
                                        "Defaults loaded into the editor. Choose Apply to save them.",
                                        "Значения по умолчанию загружены в редактор. Нажмите «Применить», чтобы сохранить их.",
                                    ),
                                );
                            }
                            Err(error) => self.set_banner(
                                BannerKind::Error,
                                format!(
                                    "{}: {error}",
                                    language.text(
                                        "Could not load product defaults",
                                        "Не удалось загрузить настройки по умолчанию"
                                    )
                                ),
                            ),
                        }
                    }
                    if ui
                        .button(language.text(
                            "Keep current settings",
                            "Оставить текущие настройки",
                        ))
                        .clicked()
                    {
                        self.confirmation = Confirmation::None;
                    }
                });
            });
        ui.add_space(8.0);
    }

    fn discard_panels(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        let language = self.language;
        if self.confirmation == Confirmation::CancelChanges {
            confirmation_frame(ui, |ui| {
                ui.label(
                    RichText::new(language.text(
                        "Discard every unsaved change in the editor?",
                        "Отменить все несохранённые изменения в редакторе?",
                    ))
                    .strong()
                    .color(Color32::WHITE),
                );
                ui.label(
                    RichText::new(language.text(
                        "The saved configuration on disk will remain unchanged.",
                        "Сохранённая на диске конфигурация не изменится.",
                    ))
                    .color(Color32::WHITE),
                );
                ui.horizontal(|ui| {
                    if ui
                        .button(
                            language.text("Discard editor changes", "Отменить изменения редактора"),
                        )
                        .clicked()
                    {
                        self.config.clone_from(&self.persisted);
                        self.tray_autostart = self.persisted_tray_autostart;
                        self.confirmation = Confirmation::None;
                        self.set_banner(
                            BannerKind::Information,
                            language.text(
                                "Unsaved changes were cancelled.",
                                "Несохранённые изменения отменены.",
                            ),
                        );
                    }
                    if ui
                        .button(language.text("Keep editing", "Продолжить редактирование"))
                        .clicked()
                    {
                        self.confirmation = Confirmation::None;
                    }
                });
            });
            ui.add_space(8.0);
        }
        if self.confirmation == Confirmation::Close {
            confirmation_frame(ui, |ui| {
                ui.label(
                    RichText::new(language.text(
                        "Close and permanently discard unsaved changes?",
                        "Закрыть окно и безвозвратно отменить несохранённые изменения?",
                    ))
                    .strong()
                    .color(Color32::WHITE),
                );
                ui.label(
                    RichText::new(language.text(
                        "Choose Apply first if you want to keep these edits.",
                        "Если изменения нужно сохранить, сначала нажмите «Применить».",
                    ))
                    .color(Color32::WHITE),
                );
                ui.horizontal(|ui| {
                    if ui
                        .button(
                            language
                                .text("Discard changes and close", "Отменить изменения и закрыть"),
                        )
                        .clicked()
                    {
                        self.allow_close = true;
                        context.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui
                        .button(language.text("Keep editing", "Продолжить редактирование"))
                        .clicked()
                    {
                        self.confirmation = Confirmation::None;
                    }
                });
            });
            ui.add_space(8.0);
        }
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        let language = self.language;
        ui.separator();
        self.restore_panel(ui);
        self.discard_panels(ui, context);
        ui.horizontal(|ui| {
            if ui
                .button(language.text("Reload from disk", "Перезагрузить с диска"))
                .clicked()
            {
                self.reload_from_disk();
            }
            if ui
                .button(language.text(
                    "Restore defaults...",
                    "Восстановить значения по умолчанию...",
                ))
                .clicked()
            {
                self.confirmation = Confirmation::RestoreDefaults;
            }
            if ui
                .add_enabled(
                    self.is_dirty(),
                    egui::Button::new(language.text("Cancel changes", "Отменить изменения")),
                )
                .clicked()
            {
                self.confirmation = Confirmation::CancelChanges;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(language.text("Close", "Закрыть")).clicked() {
                    if self.is_dirty() {
                        self.confirmation = Confirmation::Close;
                    } else {
                        self.allow_close = true;
                        context.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                if ui
                    .add_enabled(
                        (self.is_dirty()
                            || self.service_reload_state == ServiceReloadState::RetryRequired)
                            && self.pending_reload.is_none(),
                        egui::Button::new(language.text("Apply", "Применить")),
                    )
                    .clicked()
                {
                    self.apply();
                }
            });
        });
    }
}

fn ensure_exact_rule_draft(config: &mut ControllerConfig, image: &str) -> bool {
    if config
        .rules
        .iter()
        .any(|rule| rule.image.eq_ignore_ascii_case(image))
    {
        return false;
    }
    config.rules.push(ProcessRule {
        image: image.to_owned(),
        mode: RuleMode::Auto,
        profile: WorkloadProfile::Balanced,
        group: None,
        llc: None,
    });
    true
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

impl eframe::App for SettingsApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_activation(context);
        if context.input(|input| input.viewport().close_requested())
            && self.is_dirty()
            && !self.allow_close
        {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirmation = Confirmation::Close;
        }
        self.poll_reload(context);
        self.poll_diagnostic(context);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = ui.ctx().clone();
        egui::CentralPanel::default().show(ui, |ui| {
            self.top_bar(ui);
            self.banner(ui);
            egui::Panel::bottom("settings-actions")
                .resizable(false)
                .show(ui, |ui| self.bottom_bar(ui, &context));
            egui::CentralPanel::default().show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.tab {
                        SettingsTab::General => self.general_tab(ui),
                        SettingsTab::Adaptive => self.adaptive_tab(ui),
                        SettingsTab::Responsiveness => self.responsiveness_tab(ui),
                        SettingsTab::BackgroundEfficiency => {
                            self.background_efficiency_tab(ui);
                        }
                        SettingsTab::Rules => self.rules_tab(ui),
                        SettingsTab::Logging => self.logging_tab(ui),
                        SettingsTab::Diagnostics => self.diagnostics_tab(ui),
                    });
            });
        });
    }
}

fn latency_guard_toggle(
    ui: &mut egui::Ui,
    enabled: &mut bool,
    controls_enabled: bool,
    language: Language,
) {
    let help = language.text(
        "Measures normal-priority scheduler wake lateness. Sustained p99, DPC, or interrupt pressure can reduce Memory-profile concurrency within the configured bounds.",
        "Измеряет задержку пробуждения планировщика с обычным приоритетом. Устойчивое давление p99, DPC или прерываний может уменьшить параллелизм Memory-профиля в заданных границах.",
    );
    ui.add_enabled(
        controls_enabled,
        egui::Checkbox::new(
            enabled,
            language.text(
                "Enable scheduling latency guard",
                "Включить контроль задержки планирования",
            ),
        ),
    )
    .on_hover_text(help);
}

const fn reserve_help(language: Language) -> &'static str {
    language.text(
        "Excludes complete physical-core CPU Sets from managed application plans while leaving Windows and protected system processes unrestricted.",
        "Исключает CPU Sets целых физических ядер из планов управляемых приложений, не ограничивая Windows и защищённые системные процессы.",
    )
}

fn general_values(ui: &mut egui::Ui, config: &mut ControllerConfig, language: Language) {
    egui::Grid::new("general-values")
        .num_columns(2)
        .spacing([18.0, 10.0])
        .show(ui, |ui| {
            let schema_help = match language {
                Language::English => format!(
                    "The service currently supports schema version {CONFIG_SCHEMA_VERSION}. This field is read-only."
                ),
                Language::Russian => format!(
                    "Служба сейчас поддерживает схему версии {CONFIG_SCHEMA_VERSION}. Это поле доступно только для чтения."
                ),
            };
            let schema_label = ui
                .label(language.text("Configuration schema version", "Версия схемы конфигурации"))
                .on_hover_text(&schema_help);
            ui.add_enabled(false, egui::DragValue::new(&mut config.schema_version))
                .labelled_by(schema_label.id)
                .on_hover_text(&schema_help);
            ui.end_row();

            let sample_help = language.text(
                "How often the service samples process and CPU activity. Lower values react faster but add more telemetry work.",
                "Как часто служба измеряет активность процессов и CPU. Меньшие значения ускоряют реакцию, но увеличивают объём телеметрии.",
            );
            let sample_label = ui.label(language.text(
                "Sample interval (milliseconds)",
                "Интервал опроса (миллисекунды)",
            )).on_hover_text(sample_help);
            ui.add(
                egui::DragValue::new(&mut config.sample_interval_ms)
                    .range(1_000..=60_000)
                    .speed(100),
            )
            .labelled_by(sample_label.id)
            .on_hover_text(sample_help);
            ui.end_row();

            let utilization_help = language.text(
                "Minimum CPU activity required before an implicitly scoped process is managed. 100 basis points equal 1% of one logical CPU.",
                "Минимальная активность CPU для управления неявно выбранным процессом. 100 базисных пунктов равны 1% одного логического CPU.",
            );
            let utilization_label = ui.label(language.text(
                "Minimum process utilization (basis points)",
                "Минимальная загрузка процесса (базисные пункты)",
            )).on_hover_text(utilization_help);
            ui.add(
                egui::DragValue::new(&mut config.minimum_process_utilization_bps)
                    .range(0..=10_000)
                    .speed(25),
            )
            .labelled_by(utilization_label.id)
            .on_hover_text(utilization_help);
            ui.end_row();

            ui.label(language.text(
                "Default process rule mode",
                "Режим правила процессов по умолчанию",
            ))
            .on_hover_text(rule_mode_help(language));
            rule_mode_combo(
                ui,
                "default-rule-mode",
                language.text(
                    "Default process rule mode",
                    "Режим правила процессов по умолчанию",
                ),
                &mut config.default_rule_mode,
                language,
                false,
            );
            ui.end_row();

            ui.label(language.text("Default workload profile", "Профиль нагрузки по умолчанию"))
                .on_hover_text(workload_profile_help(language));
            workload_profile_combo(
                ui,
                "default-workload-profile",
                language.text("Default workload profile", "Профиль нагрузки по умолчанию"),
                &mut config.default_workload_profile,
                language,
            );
            ui.end_row();
        });
}

fn process_rule_ui(
    ui: &mut egui::Ui,
    index: usize,
    rule: &mut ProcessRule,
    language: Language,
    scroll_to: bool,
) -> bool {
    let mut remove = false;
    let group = ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "{} {}",
                    language.text("Rule", "Правило"),
                    index + 1
                ))
                .strong(),
            );
            remove = ui
                .button(format!(
                    "{} {}",
                    language.text("Remove rule", "Удалить правило"),
                    index + 1
                ))
                .clicked();
        });
        process_rule_grid(ui, index, rule, language);
        if rule.mode == RuleMode::Strict {
            ui.label(language.text(
                "Strict pins the process to the specified processor group and cache domain.",
                "Строгий режим закрепляет процесс за указанной группой процессоров и доменом кеша.",
            ));
        }
    });
    if scroll_to {
        group.response.scroll_to_me(Some(egui::Align::Center));
    }
    remove
}

fn process_rule_grid(ui: &mut egui::Ui, index: usize, rule: &mut ProcessRule, language: Language) {
    egui::Grid::new(("process-rule", index))
        .num_columns(2)
        .spacing([14.0, 8.0])
        .show(ui, |ui| {
            process_image_row(ui, rule, language);
            process_mode_row(ui, index, rule, language);
            process_profile_row(ui, index, rule, language);
            if rule.mode == RuleMode::Strict {
                strict_domain_rows(ui, rule, language);
            }
        });
}

fn process_image_row(ui: &mut egui::Ui, rule: &mut ProcessRule, language: Language) {
    let help = language.text(
        "Enter only the executable file name, for example game.exe. Matching is exact and case-insensitive; paths and wildcards are rejected.",
        "Укажите только имя исполняемого файла, например game.exe. Сопоставление точное и без учёта регистра; пути и маски не допускаются.",
    );
    let label = ui
        .label(language.text("Executable image name", "Имя исполняемого файла"))
        .on_hover_text(help);
    ui.add(
        egui::TextEdit::singleline(&mut rule.image)
            .hint_text("example.exe")
            .desired_width(260.0),
    )
    .labelled_by(label.id)
    .on_hover_text(help);
    ui.end_row();
}

fn process_mode_row(ui: &mut egui::Ui, index: usize, rule: &mut ProcessRule, language: Language) {
    ui.label(language.text("Placement mode", "Режим размещения"))
        .on_hover_text(rule_mode_help(language));
    let previous = rule.mode;
    rule_mode_combo(
        ui,
        ("process-rule-mode", index),
        &format!(
            "{} {} — {}",
            language.text("Rule", "Правило"),
            index + 1,
            language.text("placement mode", "режим размещения")
        ),
        &mut rule.mode,
        language,
        true,
    );
    if rule.mode != previous {
        if rule.mode == RuleMode::Strict {
            rule.group.get_or_insert(0);
            rule.llc.get_or_insert(0);
        } else {
            rule.group = None;
            rule.llc = None;
        }
    }
    ui.end_row();
}

fn process_profile_row(
    ui: &mut egui::Ui,
    index: usize,
    rule: &mut ProcessRule,
    language: Language,
) {
    ui.label(language.text("Workload profile", "Профиль нагрузки"))
        .on_hover_text(workload_profile_help(language));
    workload_profile_combo(
        ui,
        ("process-workload-profile", index),
        &format!(
            "{} {} — {}",
            language.text("Rule", "Правило"),
            index + 1,
            language.text("workload profile", "профиль нагрузки")
        ),
        &mut rule.profile,
        language,
    );
    ui.end_row();
}

fn strict_domain_rows(ui: &mut egui::Ui, rule: &mut ProcessRule, language: Language) {
    let help = language.text(
        "Strict mode uses this Windows processor group and LLC index exactly. Use winsched topology to discover valid values.",
        "Строгий режим точно использует эту группу процессоров Windows и индекс LLC. Допустимые значения можно узнать командой winsched topology.",
    );
    let group_label = ui
        .label(language.text("Processor group", "Группа процессоров"))
        .on_hover_text(help);
    ui.add(egui::DragValue::new(rule.group.get_or_insert(0)))
        .labelled_by(group_label.id)
        .on_hover_text(help);
    ui.end_row();
    let llc_label = ui
        .label(language.text("Last-level cache index", "Индекс кеша последнего уровня"))
        .on_hover_text(help);
    ui.add(egui::DragValue::new(rule.llc.get_or_insert(0)))
        .labelled_by(llc_label.id)
        .on_hover_text(help);
    ui.end_row();
}

fn read_status(path: &Path) -> Option<ControllerStatus> {
    let contents = fs::read(path).ok()?;
    serde_json::from_slice(&contents).ok()
}

fn diagnostic_metrics(ui: &mut egui::Ui, report: &DiagnosticReport, language: Language) {
    ui.heading(language.text("Measurements", "Измерения"));
    let system = report.system;
    let shell = report.shell;
    let taskbar = shell.taskbar;
    egui::Grid::new("diagnostic-measurements")
        .num_columns(2)
        .spacing([18.0, 8.0])
        .show(ui, |ui| {
            ui.label(language.text("Average CPU utilization", "Средняя загрузка CPU"));
            ui.label(format!(
                "{}.{:02}%",
                system.average_cpu_utilization_bps / 100,
                system.average_cpu_utilization_bps % 100
            ));
            ui.end_row();
            ui.label(language.text("Maximum processor queue", "Максимальная очередь CPU"));
            ui.label(system.maximum_processor_queue_length.to_string());
            ui.end_row();
            ui.label(language.text("Scheduler wake p99", "Пробуждение планировщика p99"));
            ui.label(format!("{} us", system.scheduler_latency.p99_lateness_us));
            ui.end_row();
            ui.label(language.text("Maximum DPC / interrupt", "Максимум DPC / прерываний"));
            ui.label(format!(
                "{}.{:02}% / {}.{:02}%",
                system.maximum_dpc_time_bps / 100,
                system.maximum_dpc_time_bps % 100,
                system.maximum_interrupt_time_bps / 100,
                system.maximum_interrupt_time_bps % 100
            ));
            ui.end_row();
            ui.label(language.text("Minimum available memory", "Минимум доступной памяти"));
            ui.label(format!(
                "{} GiB / {} GiB",
                format_gib(system.minimum_available_memory_bytes),
                format_gib(system.total_physical_memory_bytes)
            ));
            ui.end_row();
            ui.label(language.text("Taskbar response p50 / p95", "Ответ taskbar p50 / p95"));
            ui.label(format!(
                "{} / {} us; {} timeouts",
                taskbar.p50_response_us, taskbar.p95_response_us, taskbar.timeout_samples
            ));
            ui.end_row();
            ui.label(language.text("Explorer processes / windows", "Процессы / окна Explorer"));
            ui.label(format!(
                "{} / {} ({} threads)",
                shell.explorer_processes, shell.explorer_windows, shell.explorer_threads
            ));
            ui.end_row();
            ui.label(language.text("Separate Explorer processes", "Отдельные процессы Explorer"));
            ui.label(match shell.launch_folders_in_separate_process {
                Some(true) => language.text("enabled", "включены"),
                Some(false) => language.text("disabled", "выключены"),
                None => language.text("unknown", "неизвестно"),
            });
            ui.end_row();
            ui.label(language.text("WSL / VMware VM processes", "Процессы WSL / VMware VM"));
            ui.label(format!(
                "{} / {}",
                report.virtualization.wsl_processes, report.virtualization.vmware_vm_processes
            ));
            ui.end_row();
            ui.label(language.text(".wslconfig", ".wslconfig"));
            ui.label(if report.virtualization.wsl_config.present {
                language.text("present (read-only analysis)", "найден (только чтение)")
            } else {
                language.text(
                    "not present; WSL defaults apply",
                    "не найден; действуют настройки WSL по умолчанию",
                )
            });
            ui.end_row();
            let advice = report.virtualization.wsl_advice;
            if advice.resource_pressure_observed {
                ui.label(language.text("WSL advisory", "Рекомендация WSL"));
                let memory = advice
                    .recommended_memory_bytes
                    .map(format_gib)
                    .map_or_else(|| "-".to_owned(), |value| format!("{value} GiB"));
                let processors = advice
                    .recommended_processors
                    .map_or_else(|| "-".to_owned(), |value| value.to_string());
                ui.label(match language {
                    Language::English => format!(
                        "Review only; memory {memory}, processors {processors}. No automatic changes."
                    ),
                    Language::Russian => format!(
                        "Только для проверки: память {memory}, процессоры {processors}. Автоматических изменений нет."
                    ),
                });
                ui.end_row();
            }
        });
}

fn format_gib(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    let whole = bytes / GIB;
    let tenth = bytes % GIB * 10 / GIB;
    format!("{whole}.{tenth}")
}

const fn diagnostic_finding_text(
    code: DiagnosticFindingCode,
    language: Language,
) -> (&'static str, &'static str) {
    match code {
        DiagnosticFindingCode::Healthy => (
            language.text(
                "No supported responsiveness pressure signal was detected.",
                "Поддерживаемые признаки давления на отзывчивость не обнаружены.",
            ),
            language.text(
                "Repeat the diagnostic while the symptom is occurring if delays persist.",
                "Если задержки сохраняются, повторите диагностику непосредственно во время проблемы.",
            ),
        ),
        DiagnosticFindingCode::CpuSaturation => (
            language.text(
                "CPU capacity or runnable-queue pressure is elevated.",
                "Повышена загрузка CPU или очередь готовых потоков.",
            ),
            language.text(
                "Reduce or contain compute-heavy workloads before changing shell placement.",
                "Сначала ограничьте вычислительные нагрузки, не меняя размещение оболочки.",
            ),
        ),
        DiagnosticFindingCode::SchedulerLatency => (
            language.text(
                "Normal-priority scheduler wake latency is elevated.",
                "Повышена задержка пробуждения потоков обычного приоритета.",
            ),
            language.text(
                "Inspect sustained CPU, virtualization, DPC, and interrupt pressure.",
                "Проверьте устойчивую нагрузку CPU, виртуализацию, DPC и прерывания.",
            ),
        ),
        DiagnosticFindingCode::DpcOrInterruptPressure => (
            language.text(
                "DPC or interrupt processing is elevated.",
                "Повышена нагрузка DPC или аппаратных прерываний.",
            ),
            language.text(
                "Investigate drivers and devices before applying CPU Set changes.",
                "Проверьте драйверы и устройства до изменения CPU Sets.",
            ),
        ),
        DiagnosticFindingCode::MemoryPressure => (
            language.text(
                "Available physical memory is low.",
                "Доступной физической памяти осталось мало.",
            ),
            language.text(
                "Reduce memory pressure and inspect hard-fault activity.",
                "Уменьшите давление на память и проверьте hard faults.",
            ),
        ),
        DiagnosticFindingCode::ShellLatencyWithSpareCpu => (
            language.text(
                "The taskbar is slow while CPU capacity remains available.",
                "Панель задач отвечает медленно при наличии свободного CPU.",
            ),
            language.text(
                "Inspect Explorer integrations and GUI clients; more reserved cores are unlikely to help.",
                "Проверьте интеграции Explorer и GUI-клиенты; дополнительный резерв ядер вряд ли поможет.",
            ),
        ),
        DiagnosticFindingCode::ExplorerFanout => (
            language.text(
                "Many Explorer processes or folder windows are active.",
                "Активно много процессов Explorer или окон папок.",
            ),
            language.text(
                "Treat this only as context; test fewer windows or shell extensions without changing SeparateProcess automatically.",
                "Считайте это только контекстом: проверьте меньше окон или расширений, не меняя SeparateProcess автоматически.",
            ),
        ),
        DiagnosticFindingCode::WslResourcePressure => (
            language.text(
                "WSL is active while the host is under measurable resource pressure.",
                "WSL активен при измеримом давлении ресурсов на хосте.",
            ),
            language.text(
                "Review .wslconfig limits; never apply process CPU Sets to vmmemWSL.",
                "Проверьте ограничения .wslconfig; никогда не применяйте CPU Sets процесса к vmmemWSL.",
            ),
        ),
        DiagnosticFindingCode::ServiceStatusUnavailable => (
            language.text(
                "WinSched service status is unavailable.",
                "Статус службы WinSched недоступен.",
            ),
            language.text(
                "Start or update the service to include live policy context.",
                "Запустите или обновите службу для получения данных действующей политики.",
            ),
        ),
    }
}

fn confirmation_frame(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(Color32::from_rgb(112, 48, 42))
        .corner_radius(5.0)
        .inner_margin(10.0)
        .show(ui, contents);
}

fn rule_mode_name(mode: RuleMode, language: Language) -> &'static str {
    match mode {
        RuleMode::Off => language.text("Off", "Выключен"),
        RuleMode::Sticky => language.text("Sticky", "Закреплённый"),
        RuleMode::Auto => language.text("Auto", "Авто"),
        RuleMode::Performance => language.text("Performance", "Производительность"),
        RuleMode::Efficiency => language.text("Efficiency", "Эффективность"),
        RuleMode::Strict => language.text("Strict domain", "Строгий домен"),
    }
}

fn rule_mode_combo(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    accessible_label: &str,
    mode: &mut RuleMode,
    language: Language,
    allow_strict: bool,
) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(format!(
            "{accessible_label}: {}",
            rule_mode_name(*mode, language)
        ))
        .show_ui(ui, |ui| {
            for candidate in [
                RuleMode::Off,
                RuleMode::Sticky,
                RuleMode::Auto,
                RuleMode::Performance,
                RuleMode::Efficiency,
                RuleMode::Strict,
            ] {
                if candidate == RuleMode::Strict && !allow_strict {
                    continue;
                }
                ui.selectable_value(mode, candidate, rule_mode_name(candidate, language));
            }
        })
        .response
        .on_hover_text(rule_mode_help(language));
}

const fn rule_mode_help(language: Language) -> &'static str {
    language.text(
        "Controls placement behavior: Off excludes the process, Sticky keeps its first assignment, Auto may move it after policy safeguards, Performance/Efficiency filter CPU classes, and Strict targets one LLC domain.",
        "Определяет размещение: «Выключен» исключает процесс, Sticky сохраняет первое назначение, Auto может перемещать после проверок политики, Performance/Efficiency фильтруют классы CPU, а Strict выбирает один домен LLC.",
    )
}

fn workload_profile_name(profile: WorkloadProfile, language: Language) -> &'static str {
    match profile {
        WorkloadProfile::Interactive => language.text("Interactive", "Интерактивный"),
        WorkloadProfile::Memory => language.text("Memory-bound", "Ограничен памятью"),
        WorkloadProfile::Compute => language.text("Compute", "Вычислительный"),
        WorkloadProfile::Background => language.text("Background", "Фоновый"),
        WorkloadProfile::Balanced => language.text("Balanced", "Сбалансированный"),
    }
}

fn workload_profile_combo(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    accessible_label: &str,
    profile: &mut WorkloadProfile,
    language: Language,
) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(format!(
            "{accessible_label}: {}",
            workload_profile_name(*profile, language)
        ))
        .show_ui(ui, |ui| {
            for candidate in [
                WorkloadProfile::Interactive,
                WorkloadProfile::Memory,
                WorkloadProfile::Compute,
                WorkloadProfile::Background,
                WorkloadProfile::Balanced,
            ] {
                ui.selectable_value(
                    profile,
                    candidate,
                    workload_profile_name(candidate, language),
                );
            }
        })
        .response
        .on_hover_text(workload_profile_help(language));
}

const fn workload_profile_help(language: Language) -> &'static str {
    language.text(
        "Interactive stays on one LLC, Memory spreads one thread per physical core by default, Compute uses both SMT siblings, and Background can opt an exact rule into reversible EcoQoS/memory handling. Balanced retains standard LLC-aware adaptive behavior.",
        "Interactive остаётся в одном LLC, Memory по умолчанию распределяет по одному потоку на физическое ядро, Compute использует оба SMT-потока, а Background может включить для точного правила обратимое управление EcoQoS/памятью. Balanced сохраняет обычное адаптивное LLC-размещение.",
    )
}

fn background_efficiency_toggle(
    ui: &mut egui::Ui,
    value: &mut bool,
    enabled: bool,
    label: &str,
    explanation: &str,
) {
    ui.add_enabled(enabled, egui::Checkbox::new(value, label))
        .on_hover_text(explanation);
}

fn responsiveness_value_u8(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut u8,
    range: std::ops::RangeInclusive<u8>,
    enabled: bool,
    explanation: &str,
    suffix: &str,
) {
    let row_width = (ui.available_width() - 16.0).max(240.0);
    ui.group(|ui| {
        ui.set_width(row_width);
        let response = ui
            .label(RichText::new(label).strong())
            .on_hover_text(explanation);
        ui.label(explanation).on_hover_text(explanation);
        ui.add_space(4.0);
        ui.add_enabled(
            enabled,
            egui::DragValue::new(value).range(range).suffix(suffix),
        )
        .labelled_by(response.id)
        .on_hover_text(explanation);
    });
    ui.add_space(6.0);
}

fn responsiveness_value_u16(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut u16,
    range: std::ops::RangeInclusive<u16>,
    enabled: bool,
    explanation: &str,
) {
    let row_width = (ui.available_width() - 16.0).max(240.0);
    ui.group(|ui| {
        ui.set_width(row_width);
        let response = ui
            .label(RichText::new(label).strong())
            .on_hover_text(explanation);
        ui.label(explanation).on_hover_text(explanation);
        ui.add_space(4.0);
        ui.add_enabled(enabled, egui::DragValue::new(value).range(range))
            .labelled_by(response.id)
            .on_hover_text(explanation);
    });
    ui.add_space(6.0);
}

fn responsiveness_value_u64(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut u64,
    range: std::ops::RangeInclusive<u64>,
    enabled: bool,
    explanation: &str,
) {
    let row_width = (ui.available_width() - 16.0).max(240.0);
    ui.group(|ui| {
        ui.set_width(row_width);
        let response = ui
            .label(RichText::new(label).strong())
            .on_hover_text(explanation);
        ui.label(explanation).on_hover_text(explanation);
        ui.add_space(4.0);
        ui.add_enabled(
            enabled,
            egui::DragValue::new(value).range(range).speed(1_000),
        )
        .labelled_by(response.id)
        .on_hover_text(explanation);
    });
    ui.add_space(6.0);
}

fn policy_value_u16(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut u16,
    range: std::ops::RangeInclusive<u16>,
    explanation: &str,
) {
    let row_width = (ui.available_width() - 16.0).max(240.0);
    ui.group(|ui| {
        ui.set_width(row_width);
        let response = ui
            .label(RichText::new(label).strong())
            .on_hover_text(explanation);
        ui.label(explanation).on_hover_text(explanation);
        ui.add_space(4.0);
        ui.add_sized([180.0, 24.0], egui::DragValue::new(value).range(range))
            .labelled_by(response.id)
            .on_hover_text(explanation);
    });
    ui.add_space(6.0);
}

fn policy_value_u64(ui: &mut egui::Ui, label: &str, value: &mut u64, explanation: &str) {
    let row_width = (ui.available_width() - 16.0).max(240.0);
    ui.group(|ui| {
        ui.set_width(row_width);
        let response = ui
            .label(RichText::new(label).strong())
            .on_hover_text(explanation);
        ui.label(explanation).on_hover_text(explanation);
        ui.add_space(4.0);
        ui.add_sized([180.0, 24.0], egui::DragValue::new(value).speed(100))
            .labelled_by(response.id)
            .on_hover_text(explanation);
    });
    ui.add_space(6.0);
}

fn logging_value_u16(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut u16,
    enabled: bool,
    explanation: &str,
    suffix: &str,
) {
    let row_width = (ui.available_width() - 16.0).max(240.0);
    ui.group(|ui| {
        ui.set_width(row_width);
        let response = ui
            .label(RichText::new(label).strong())
            .on_hover_text(explanation);
        ui.label(explanation).on_hover_text(explanation);
        ui.add_space(4.0);
        ui.add_enabled(
            enabled,
            egui::DragValue::new(value)
                .range(MIN_LOG_FILE_SIZE_MIB..=MAX_LOG_FILE_SIZE_MIB)
                .suffix(suffix),
        )
        .labelled_by(response.id)
        .on_hover_text(explanation);
    });
    ui.add_space(6.0);
}

fn logging_value_u8(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut u8,
    enabled: bool,
    explanation: &str,
) {
    let row_width = (ui.available_width() - 16.0).max(240.0);
    ui.group(|ui| {
        ui.set_width(row_width);
        let response = ui
            .label(RichText::new(label).strong())
            .on_hover_text(explanation);
        ui.label(explanation).on_hover_text(explanation);
        ui.add_space(4.0);
        ui.add_enabled(
            enabled,
            egui::DragValue::new(value).range(0..=MAX_RETAINED_LOG_ARCHIVES),
        )
        .labelled_by(response.id)
        .on_hover_text(explanation);
    });
    ui.add_space(6.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_rule_handoff_creates_one_unsaved_case_insensitive_draft() {
        let mut config = ControllerConfig::default();
        assert!(ensure_exact_rule_draft(&mut config, "worker.exe"));
        assert_eq!(config.rules.len(), 1);
        assert_eq!(config.rules[0].image, "worker.exe");
        assert_eq!(config.rules[0].mode, RuleMode::Auto);
        assert_eq!(config.rules[0].profile, WorkloadProfile::Balanced);
        assert!(!ensure_exact_rule_draft(&mut config, "WORKER.EXE"));
        assert_eq!(config.rules.len(), 1);
    }

    #[test]
    fn rule_handoff_rejects_paths_and_empty_names() {
        assert!(validate_rule_image(OsString::from("game.exe")).is_ok());
        assert!(validate_rule_image(OsString::from("C:\\game.exe")).is_err());
        assert!(validate_rule_image(OsString::from("../game.exe")).is_err());
        assert!(validate_rule_image(OsString::from("  ")).is_err());
    }
}
