use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui::{self, Color32, RichText};
use winsched_config::{
    CONFIG_SCHEMA_VERSION, ControllerConfig, ControllerMode, LoggingConfig,
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
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn run() -> Result<(), Box<dyn Error>> {
    let paths = SettingsPaths::discover();
    let _instance = InstanceLock::acquire(&paths.instance_lock)?;
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
            Ok(Box::new(SettingsApp::new(paths, config, language)))
        }),
    )?;
    Ok(())
}

pub fn show_startup_error(message: &str) {
    let detail = format!(
        "WinSched Settings could not start. / Не удалось запустить настройки WinSched.\n\n{message}"
    );
    let _ = std::process::Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Add-Type -AssemblyName System.Windows.Forms; [void][System.Windows.Forms.MessageBox]::Show($env:WINSCHED_SETTINGS_STARTUP_ERROR, 'WinSched Settings / Настройки WinSched', [System.Windows.Forms.MessageBoxButtons]::OK, [System.Windows.Forms.MessageBoxIcon]::Error)",
        ])
        .env("WINSCHED_SETTINGS_STARTUP_ERROR", detail)
        .creation_flags(CREATE_NO_WINDOW)
        .status();
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
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Globalization.CultureInfo]::CurrentUICulture.Name",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    if output.is_ok_and(|output| {
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .to_ascii_lowercase()
            .starts_with("ru")
    }) {
        Language::Russian
    } else {
        Language::English
    }
}

struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    fn acquire(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(path)
            .map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!(
                        "another WinSched Settings window is already open, or the lock is unavailable: {error}"
                    ),
                )
            })?;
        Ok(Self { _file: file })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    General,
    Adaptive,
    Responsiveness,
    Rules,
    Logging,
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
    language: Language,
}

impl SettingsApp {
    fn new(paths: SettingsPaths, config: ControllerConfig, language: Language) -> Self {
        let tray_autostart = tray_autostart_enabled(&paths.tray_startup_shortcut);
        Self {
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
            language,
        }
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
        ui.horizontal(|ui| {
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
                SettingsTab::Rules,
                language.text("Process rules", "Правила процессов"),
            );
            ui.selectable_value(
                &mut self.tab,
                SettingsTab::Logging,
                language.text("Logging", "Журнал"),
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
            ui.label(RichText::new(language.text("Controller mode", "Режим контроллера")).strong());
            ui.radio_value(
                &mut self.config.controller_mode,
                ControllerMode::Off,
                language.text(
                    "Off — do not observe or change process placement",
                    "Выключен — не наблюдать и не изменять размещение процессов",
                ),
            );
            ui.radio_value(
                &mut self.config.controller_mode,
                ControllerMode::Observe,
                language.text(
                    "Observe — calculate and report decisions without applying them",
                    "Наблюдение — рассчитывать и показывать решения без их применения",
                ),
            );
            ui.radio_value(
                &mut self.config.controller_mode,
                ControllerMode::Auto,
                language.text(
                    "Auto — apply validated CPU placement decisions",
                    "Авто — применять проверенные решения по размещению на CPU",
                ),
            );
        });
        ui.add_space(10.0);
        general_values(ui, &mut self.config, language);
        ui.add_space(8.0);
        ui.checkbox(
            &mut self.config.all_user_processes,
            language.text(
                "Manage all eligible user processes, not only explicitly listed rules",
                "Управлять всеми подходящими пользовательскими процессами, а не только указанными в правилах",
            ),
        );
        ui.label(language.text(
            "When disabled, a process must have an exact executable-name rule before WinSched considers it.",
            "Если флажок снят, WinSched рассматривает процесс только при наличии правила с точным именем исполняемого файла.",
        ));
        ui.add_space(10.0);
        ui.checkbox(
            &mut self.tray_autostart,
            language.text(
                "Start the WinSched tray automatically when a user signs in",
                "Автоматически запускать WinSched в области уведомлений при входе пользователя",
            ),
        );
        ui.label(language.text(
            "This manages the machine-wide WinSched Tray shortcut in the Windows Startup folder.",
            "Этот параметр управляет общесистемным ярлыком WinSched Tray в папке автозагрузки Windows.",
        ));
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

    fn logging_tab(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.heading(language.text("Diagnostic logging", "Диагностический журнал"));
        ui.label(language.text(
            "Control how much service diagnostic history is kept on this computer.",
            "Настройте объём истории диагностики службы, сохраняемой на этом компьютере.",
        ));
        ui.add_space(8.0);
        ui.checkbox(
            &mut self.config.logging.enabled,
            language.text(
                "Enable detailed service logging",
                "Включить подробный журнал службы",
            ),
        );
        ui.label(language.text(
            "Diagnostic events are written as JSON lines to:",
            "Диагностические события записываются строками JSON в:",
        ));
        ui.monospace(self.paths.log.display().to_string());
        ui.add_space(8.0);

        if !self.config.logging.enabled {
            ui.group(|ui| {
                ui.label(language.text(
                    "Detailed logging is off. Existing log and archive files are preserved, and no new diagnostic events are written.",
                    "Подробный журнал выключен. Существующие файлы журнала и архивы сохраняются, новые диагностические события не записываются.",
                ));
            });
            ui.add_space(8.0);
        }

        logging_value_u16(
            ui,
            language.text(
                "Maximum active log size (MiB)",
                "Максимальный размер активного журнала (МиБ)",
            ),
            &mut self.config.logging.max_file_size_mib,
            self.config.logging.enabled,
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
            self.config.logging.enabled,
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
        ui.checkbox(
            &mut self.config.responsiveness.enabled,
            language.text(
                "Enable topology-aware system reserve",
                "Включить топологический системный резерв",
            ),
        );
        ui.label(language.text(
            "Protected system processes remain unrestricted and are never pinned to the reserve. The reserve is spread over LLC domains and always includes complete SMT sibling pairs.",
            "Защищённые системные процессы остаются без ограничений и не закрепляются за резервом. Резерв распределяется по LLC и всегда включает полные пары SMT-потоков.",
        ));
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
        ui.add_enabled(
            controls_enabled,
            egui::Checkbox::new(
                &mut self.config.responsiveness.latency_guard_enabled,
                language.text(
                    "Enable scheduling latency guard",
                    "Включить контроль задержки планирования",
                ),
            ),
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
        ui.checkbox(
            &mut self.config.responsiveness.memory.use_smt,
            language.text(
                "Allow both SMT threads per physical core",
                "Разрешить оба SMT-потока физического ядра",
            ),
        );
        ui.label(language.text(
            "Off is recommended for bandwidth-bound workloads on this Threadripper; the profile keeps one logical processor per physical core.",
            "Для ограниченных памятью нагрузок на этом Threadripper рекомендуется выключить: профиль оставляет один логический процессор на физическое ядро.",
        ));
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
                ui.label(language.text(
                    "Live reserve information is unavailable until a schema-3 service is running.",
                    "Информация о резерве появится после запуска службы со схемой статуса 3.",
                ));
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
        for (index, rule) in self.config.rules.iter_mut().enumerate() {
            if process_rule_ui(ui, index, rule, language) {
                remove = Some(index);
            }
            ui.add_space(8.0);
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

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

impl eframe::App for SettingsApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        if context.input(|input| input.viewport().close_requested())
            && self.is_dirty()
            && !self.allow_close
        {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirmation = Confirmation::Close;
        }
        self.poll_reload(context);
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
                        SettingsTab::Rules => self.rules_tab(ui),
                        SettingsTab::Logging => self.logging_tab(ui),
                    });
            });
        });
    }
}

fn general_values(ui: &mut egui::Ui, config: &mut ControllerConfig, language: Language) {
    egui::Grid::new("general-values")
        .num_columns(2)
        .spacing([18.0, 10.0])
        .show(ui, |ui| {
            let schema_label = ui
                .label(language.text("Configuration schema version", "Версия схемы конфигурации"));
            ui.add_enabled(false, egui::DragValue::new(&mut config.schema_version))
                .labelled_by(schema_label.id)
                .on_hover_text(match language {
                    Language::English => format!(
                        "The service currently supports schema version {CONFIG_SCHEMA_VERSION}."
                    ),
                    Language::Russian => {
                        format!("Служба сейчас поддерживает схему версии {CONFIG_SCHEMA_VERSION}.")
                    }
                });
            ui.end_row();

            let sample_label = ui.label(language.text(
                "Sample interval (milliseconds)",
                "Интервал опроса (миллисекунды)",
            ));
            ui.add(
                egui::DragValue::new(&mut config.sample_interval_ms)
                    .range(1_000..=60_000)
                    .speed(100),
            )
            .labelled_by(sample_label.id)
            .on_hover_text(language.text(
                "How often the service samples process and CPU activity.",
                "Как часто служба измеряет активность процессов и CPU.",
            ));
            ui.end_row();

            let utilization_label = ui.label(language.text(
                "Minimum process utilization (basis points)",
                "Минимальная загрузка процесса (базисные пункты)",
            ));
            ui.add(
                egui::DragValue::new(&mut config.minimum_process_utilization_bps)
                    .range(0..=10_000)
                    .speed(25),
            )
            .labelled_by(utilization_label.id)
            .on_hover_text(language.text(
                "100 basis points equal 1% CPU utilization.",
                "100 базисных пунктов равны 1% загрузки CPU.",
            ));
            ui.end_row();

            ui.label(language.text(
                "Default process rule mode",
                "Режим правила процессов по умолчанию",
            ));
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

            ui.label(language.text("Default workload profile", "Профиль нагрузки по умолчанию"));
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
) -> bool {
    let mut remove = false;
    ui.group(|ui| {
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
        egui::Grid::new(("process-rule", index))
            .num_columns(2)
            .spacing([14.0, 8.0])
            .show(ui, |ui| {
                let image_label =
                    ui.label(language.text("Executable image name", "Имя исполняемого файла"));
                ui.add(
                    egui::TextEdit::singleline(&mut rule.image)
                        .hint_text("example.exe")
                        .desired_width(260.0),
                )
                .labelled_by(image_label.id);
                ui.end_row();

                ui.label(language.text("Placement mode", "Режим размещения"));
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

                ui.label(language.text("Workload profile", "Профиль нагрузки"));
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

                if rule.mode == RuleMode::Strict {
                    let group_label =
                        ui.label(language.text("Processor group", "Группа процессоров"));
                    ui.add(egui::DragValue::new(rule.group.get_or_insert(0)))
                        .labelled_by(group_label.id);
                    ui.end_row();
                    let llc_label = ui.label(
                        language.text("Last-level cache index", "Индекс кеша последнего уровня"),
                    );
                    ui.add(egui::DragValue::new(rule.llc.get_or_insert(0)))
                        .labelled_by(llc_label.id);
                    ui.end_row();
                }
            });
        if rule.mode == RuleMode::Strict {
            ui.label(language.text(
                "Strict pins the process to the specified processor group and cache domain.",
                "Строгий режим закрепляет процесс за указанной группой процессоров и доменом кеша.",
            ));
        }
    });
    remove
}

fn read_status(path: &Path) -> Option<ControllerStatus> {
    let contents = fs::read(path).ok()?;
    serde_json::from_slice(&contents).ok()
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
        });
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
        });
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
        let response = ui.label(RichText::new(label).strong());
        ui.label(explanation);
        ui.add_space(4.0);
        ui.add_enabled(
            enabled,
            egui::DragValue::new(value).range(range).suffix(suffix),
        )
        .labelled_by(response.id);
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
        let response = ui.label(RichText::new(label).strong());
        ui.label(explanation);
        ui.add_space(4.0);
        ui.add_enabled(enabled, egui::DragValue::new(value).range(range))
            .labelled_by(response.id);
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
        let response = ui.label(RichText::new(label).strong());
        ui.label(explanation);
        ui.add_space(4.0);
        ui.add_enabled(
            enabled,
            egui::DragValue::new(value).range(range).speed(1_000),
        )
        .labelled_by(response.id);
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
        let response = ui.label(RichText::new(label).strong());
        ui.label(explanation);
        ui.add_space(4.0);
        ui.add_sized([180.0, 24.0], egui::DragValue::new(value).range(range))
            .labelled_by(response.id);
    });
    ui.add_space(6.0);
}

fn policy_value_u64(ui: &mut egui::Ui, label: &str, value: &mut u64, explanation: &str) {
    let row_width = (ui.available_width() - 16.0).max(240.0);
    ui.group(|ui| {
        ui.set_width(row_width);
        let response = ui.label(RichText::new(label).strong());
        ui.label(explanation);
        ui.add_space(4.0);
        ui.add_sized([180.0, 24.0], egui::DragValue::new(value).speed(100))
            .labelled_by(response.id);
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
        let response = ui.label(RichText::new(label).strong());
        ui.label(explanation);
        ui.add_space(4.0);
        ui.add_enabled(
            enabled,
            egui::DragValue::new(value)
                .range(MIN_LOG_FILE_SIZE_MIB..=MAX_LOG_FILE_SIZE_MIB)
                .suffix(suffix),
        )
        .labelled_by(response.id);
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
        let response = ui.label(RichText::new(label).strong());
        ui.label(explanation);
        ui.add_space(4.0);
        ui.add_enabled(
            enabled,
            egui::DragValue::new(value).range(0..=MAX_RETAINED_LOG_ARCHIVES),
        )
        .labelled_by(response.id);
    });
    ui.add_space(6.0);
}
