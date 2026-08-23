use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use winsched_config::{ControllerConfig, ControllerMode, ProcessRule, RuleMode};
use winsched_control::{ControllerStatus, STATUS_SCHEMA_VERSION};
use winsched_settings::{
    ConfigReloadLogEvent, EventLogCursor, SettingsPaths, load_config, read_config_reload_event,
    restore_defaults, save_config_atomic, set_tray_autostart, tray_autostart_enabled,
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
            .with_inner_size([840.0, 680.0])
            .with_min_inner_size([720.0, 560.0]),
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
    Rules,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BannerKind {
    Information,
    Success,
    Error,
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
    baseline_status_ms: u64,
    expected_mode: ControllerMode,
    log_cursor: EventLogCursor,
    observed_event: Option<ConfigReloadLogEvent>,
    deadline: Instant,
    next_poll: Instant,
}

enum ReloadPollOutcome {
    Pending(Duration),
    Reloaded,
    Rejected(Option<String>),
    LogReadFailed(String),
    TimedOutWithoutEvent,
    TimedOutAfterReloadEvent,
}

impl PendingReload {
    fn poll(&mut self, paths: &SettingsPaths, now: Instant) -> ReloadPollOutcome {
        if now < self.next_poll {
            return ReloadPollOutcome::Pending(self.next_poll - now);
        }
        self.next_poll = now + STATUS_POLL_INTERVAL;
        if self.observed_event.is_none() {
            match read_config_reload_event(&paths.log, &mut self.log_cursor) {
                Ok(event) => self.observed_event = event,
                Err(error) => return ReloadPollOutcome::LogReadFailed(error.to_string()),
            }
        }

        let status = read_status(&paths.status);
        if let Some(event) = &self.observed_event {
            let event_timestamp_ms = event.timestamp_ms().unwrap_or(0);
            let current_status = status.as_ref().filter(|status| {
                status.schema_version == STATUS_SCHEMA_VERSION
                    && status.updated_at_unix_ms > self.baseline_status_ms
                    && status.updated_at_unix_ms >= event_timestamp_ms
            });
            match event {
                ConfigReloadLogEvent::Reloaded { .. }
                    if current_status
                        .is_some_and(|status| status.configured_mode == self.expected_mode) =>
                {
                    return ReloadPollOutcome::Reloaded;
                }
                ConfigReloadLogEvent::Rejected { error, .. } if current_status.is_some() => {
                    let detail = current_status
                        .and_then(|status| status.last_error.clone())
                        .or_else(|| error.clone());
                    return ReloadPollOutcome::Rejected(detail);
                }
                _ => {}
            }
        }

        if now < self.deadline {
            return ReloadPollOutcome::Pending(STATUS_POLL_INTERVAL);
        }
        match &self.observed_event {
            Some(ConfigReloadLogEvent::Rejected { error, .. }) => {
                let detail = status
                    .and_then(|status| status.last_error)
                    .or_else(|| error.clone());
                ReloadPollOutcome::Rejected(detail)
            }
            Some(ConfigReloadLogEvent::Reloaded { .. }) => {
                ReloadPollOutcome::TimedOutAfterReloadEvent
            }
            None => ReloadPollOutcome::TimedOutWithoutEvent,
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

        if !config_changed {
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

        let log_cursor = match EventLogCursor::capture(&self.paths.log) {
            Ok(cursor) => cursor,
            Err(error) => {
                self.report_config_save_failure(&error, autostart_changed);
                return;
            }
        };
        let baseline_status = read_status(&self.paths.status);
        let baseline_status_ms = baseline_status
            .as_ref()
            .map_or(0, |status| status.updated_at_unix_ms);
        match save_config_atomic(&self.paths.config, &self.config) {
            Ok(validated) => {
                self.config.clone_from(&validated);
                self.persisted = validated;
                self.persisted_tray_autostart = self.tray_autostart;
                self.confirmation = Confirmation::None;
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
                    baseline_status_ms,
                    expected_mode: self.config.controller_mode,
                    log_cursor,
                    observed_event: None,
                    deadline: Instant::now()
                        + Duration::from_millis(
                            self.config.sample_interval_ms.saturating_add(5_000),
                        ),
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
            ReloadPollOutcome::LogReadFailed(error) => {
                self.pending_reload = None;
                self.set_banner(
                    BannerKind::Error,
                    format!(
                        "{}: {error}",
                        language.text(
                            "Configuration was saved, but the durable service confirmation log could not be read",
                            "Конфигурация сохранена, но не удалось прочитать журнал подтверждения службы"
                        )
                    ),
                );
            }
            ReloadPollOutcome::TimedOutWithoutEvent => {
                self.pending_reload = None;
                self.set_banner(
                    BannerKind::Information,
                    language.text(
                        "Configuration was saved, but the service did not log a reload within the expected interval. Check that the WinSched service is running.",
                        "Конфигурация сохранена, но служба не записала событие загрузки за ожидаемое время. Проверьте, что служба WinSched запущена.",
                    ),
                );
            }
            ReloadPollOutcome::TimedOutAfterReloadEvent => {
                self.pending_reload = None;
                self.set_banner(
                    BannerKind::Information,
                    language.text(
                        "The service logged a configuration reload, but status.json did not confirm the expected mode within the expected interval.",
                        "Служба записала событие загрузки конфигурации, но status.json не подтвердил ожидаемый режим за отведённое время.",
                    ),
                );
            }
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.horizontal(|ui| {
            ui.heading(language.text("WinSched Settings", "Настройки WinSched"));
            ui.separator();
            let state = if self.is_dirty() {
                RichText::new(language.text("Unsaved changes", "Есть несохранённые изменения"))
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
                SettingsTab::Rules,
                language.text("Process rules", "Правила процессов"),
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
                        self.is_dirty() && self.pending_reload.is_none(),
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
                        SettingsTab::Rules => self.rules_tab(ui),
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
                .on_hover_text(language.text(
                    "The service currently supports schema version 1.",
                    "Служба сейчас поддерживает схему версии 1.",
                ));
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
