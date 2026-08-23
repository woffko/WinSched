//! Validated, atomic persistence for the `WinSched` settings application.

#![forbid(unsafe_code)]

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use atomic_write_file::AtomicWriteFile;
use thiserror::Error;
use winsched_config::ControllerConfig;
use winsched_control::{CONFIG_FILE_NAME, INSTALL_DIRECTORY_NAME, LOG_FILE_NAME, STATUS_FILE_NAME};

const PRODUCT_DEFAULT_CONFIG: &str = include_str!("../../../config/winsched.default.toml");
const LOG_GUARD_BYTES: u64 = 256;

/// Files used by the settings application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsPaths {
    pub install_dir: PathBuf,
    pub config: PathBuf,
    pub status: PathBuf,
    pub log: PathBuf,
    pub instance_lock: PathBuf,
    pub tray_shortcut: PathBuf,
    pub tray_startup_shortcut: PathBuf,
}

impl SettingsPaths {
    /// Resolves the machine-wide `WinSched` data directory.
    #[must_use]
    pub fn discover() -> Self {
        let program_data = std::env::var_os("PROGRAMDATA")
            .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from);
        Self::under_program_data(&program_data)
    }

    /// Constructs settings paths below a supplied `ProgramData` directory.
    #[must_use]
    pub fn under_program_data(program_data: &Path) -> Self {
        let install_dir = program_data.join(INSTALL_DIRECTORY_NAME);
        let common_programs = program_data
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs");
        Self {
            config: install_dir.join(CONFIG_FILE_NAME),
            status: install_dir.join(STATUS_FILE_NAME),
            log: install_dir.join(LOG_FILE_NAME),
            instance_lock: install_dir.join("winsched-settings.lock"),
            tray_shortcut: common_programs.join("WinSched").join("WinSched.lnk"),
            tray_startup_shortcut: common_programs.join("Startup").join("WinSched Tray.lnk"),
            install_dir,
        }
    }
}

/// Durable cursor identifying the end of the event log before a settings write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventLogCursor {
    offset: u64,
    identity: Option<FileIdentity>,
    guard_start: u64,
    guard: Vec<u8>,
}

impl EventLogCursor {
    /// Captures the current log length and a rotation/truncation guard.
    ///
    /// A missing log is treated as an empty log because the service may create
    /// it after the configuration is saved.
    ///
    /// # Errors
    ///
    /// Returns an error for failures other than a missing event log.
    pub fn capture(path: &Path) -> Result<Self, SettingsError> {
        match fs::metadata(path) {
            Ok(metadata) => {
                let offset = metadata.len();
                let (guard_start, guard) = read_log_guard(path, offset)?;
                Ok(Self {
                    offset,
                    identity: Some(file_identity(&metadata)),
                    guard_start,
                    guard,
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::empty()),
            Err(error) => Err(error.into()),
        }
    }

    const fn empty() -> Self {
        Self {
            offset: 0,
            identity: None,
            guard_start: 0,
            guard: Vec::new(),
        }
    }

    #[cfg(test)]
    const fn offset(&self) -> u64 {
        self.offset
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileIdentity {
    Known {
        volume: u64,
        file: u64,
    },
    #[cfg(not(any(unix, windows)))]
    Unknown,
}

/// A durable service event proving the result of a configuration reload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigReloadLogEvent {
    Reloaded {
        timestamp_ms: Option<u64>,
    },
    Rejected {
        timestamp_ms: Option<u64>,
        error: Option<String>,
    },
}

impl ConfigReloadLogEvent {
    /// Returns the service timestamp attached to the JSONL event, when present.
    #[must_use]
    pub const fn timestamp_ms(&self) -> Option<u64> {
        match self {
            Self::Reloaded { timestamp_ms } | Self::Rejected { timestamp_ms, .. } => *timestamp_ms,
        }
    }
}

/// Reads complete JSONL records written after a captured cursor.
///
/// The cursor automatically restarts at byte zero when the current log is
/// shorter, has a different file identity, or no longer matches its trailing
/// content guard. Incomplete trailing JSONL records are left for the next poll.
///
/// # Errors
///
/// Returns an error when the log exists but cannot be inspected or read.
pub fn read_config_reload_event(
    path: &Path,
    cursor: &mut EventLogCursor,
) -> Result<Option<ConfigReloadLogEvent>, SettingsError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            *cursor = EventLogCursor::empty();
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    let identity = Some(file_identity(&metadata));
    let identity_changed = cursor.identity.is_some() && cursor.identity != identity;
    let shrunk = metadata.len() < cursor.offset;
    let guard_changed = !identity_changed && !shrunk && !log_guard_matches(path, cursor)?;
    if identity_changed || shrunk || guard_changed {
        cursor.offset = 0;
        cursor.guard_start = 0;
        cursor.guard.clear();
    }
    cursor.identity = identity;

    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(cursor.offset))?;
    let mut appended = Vec::new();
    file.read_to_end(&mut appended)?;
    let complete_len = appended
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    if complete_len == 0 {
        return Ok(None);
    }
    cursor.offset = cursor
        .offset
        .saturating_add(u64::try_from(complete_len).unwrap_or(u64::MAX));
    let (guard_start, guard) = read_log_guard(path, cursor.offset)?;
    cursor.guard_start = guard_start;
    cursor.guard = guard;

    let mut found = None;
    for line in appended[..complete_len].split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        let timestamp_ms = value
            .get("timestamp_ms")
            .and_then(serde_json::Value::as_u64);
        match value.get("event").and_then(serde_json::Value::as_str) {
            Some("config_reloaded") => {
                found = Some(ConfigReloadLogEvent::Reloaded { timestamp_ms });
            }
            Some("config_rejected_fail_closed") => {
                found = Some(ConfigReloadLogEvent::Rejected {
                    timestamp_ms,
                    error: value
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                });
            }
            _ => {}
        }
    }
    Ok(found)
}

fn read_log_guard(path: &Path, offset: u64) -> Result<(u64, Vec<u8>), SettingsError> {
    let guard_start = offset.saturating_sub(LOG_GUARD_BYTES);
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(guard_start))?;
    let mut guard = Vec::new();
    file.take(offset - guard_start).read_to_end(&mut guard)?;
    Ok((guard_start, guard))
}

fn log_guard_matches(path: &Path, cursor: &EventLogCursor) -> Result<bool, SettingsError> {
    if cursor.guard.is_empty() {
        return Ok(true);
    }
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(cursor.guard_start))?;
    let mut current = vec![0; cursor.guard.len()];
    match file.read_exact(&mut current) {
        Ok(()) => Ok(current == cursor.guard),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt as _;

    FileIdentity::Known {
        volume: metadata.dev(),
        file: metadata.ino(),
    }
}

#[cfg(windows)]
fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    use std::os::windows::fs::MetadataExt as _;

    FileIdentity::Known {
        volume: 0,
        file: metadata.creation_time(),
    }
}

#[cfg(not(any(unix, windows)))]
const fn file_identity(_metadata: &fs::Metadata) -> FileIdentity {
    FileIdentity::Unknown
}

/// Errors surfaced by settings load and save operations.
#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("failed to read or write settings: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid WinSched configuration: {0}")]
    Config(#[from] winsched_config::ConfigError),
    #[error("failed to encode WinSched configuration: {0}")]
    Encode(#[from] toml::ser::Error),
}

/// Reads and validates a configuration document.
///
/// # Errors
///
/// Returns a descriptive error for inaccessible, malformed, or invalid input.
pub fn load_config(path: &Path) -> Result<ControllerConfig, SettingsError> {
    Ok(ControllerConfig::from_toml(&fs::read_to_string(path)?)?)
}

/// Validates and renders a deterministic, human-readable TOML document.
///
/// # Errors
///
/// Returns an error when validation or serialization fails.
pub fn render_config(config: &ControllerConfig) -> Result<String, SettingsError> {
    let validated = config.clone().validate()?;
    Ok(toml::to_string_pretty(&validated)?)
}

/// Atomically replaces a configuration file after validating its complete value.
///
/// # Errors
///
/// Returns an error without replacing the previous file when validation, writing,
/// or the atomic commit fails.
pub fn save_config_atomic(
    path: &Path,
    config: &ControllerConfig,
) -> Result<ControllerConfig, SettingsError> {
    let validated = config.clone().validate()?;
    let encoded = toml::to_string_pretty(&validated)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut output = AtomicWriteFile::options().open(path)?;
    output.write_all(encoded.as_bytes())?;
    output.commit()?;
    Ok(validated)
}

/// Reports whether the common tray startup shortcut currently exists.
#[must_use]
pub fn tray_autostart_enabled(startup_shortcut: &Path) -> bool {
    startup_shortcut.is_file()
}

/// Enables or disables tray autostart using the installed Start Menu shortcut.
///
/// Enabling atomically copies the installed `WinSched.lnk` contents to the
/// common Startup directory. Disabling removes only `WinSched Tray.lnk`.
///
/// # Errors
///
/// Returns an error when the source shortcut is missing or the destination
/// cannot be atomically written or removed.
pub fn set_tray_autostart(
    source_shortcut: &Path,
    startup_shortcut: &Path,
    enabled: bool,
) -> Result<(), SettingsError> {
    if !enabled {
        match fs::remove_file(startup_shortcut) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }

    let shortcut = fs::read(source_shortcut)?;
    if let Some(parent) = startup_shortcut.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut output = AtomicWriteFile::options().open(startup_shortcut)?;
    output.write_all(&shortcut)?;
    output.commit()?;
    Ok(())
}

/// Returns the validated factory configuration shipped with the product.
///
/// # Errors
///
/// Returns an error if the compile-time embedded factory document is invalid.
pub fn restore_defaults() -> Result<ControllerConfig, SettingsError> {
    Ok(ControllerConfig::from_toml(PRODUCT_DEFAULT_CONFIG)?)
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use winsched_config::{ControllerMode, ProcessRule, RuleMode};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock must be after the Unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "winsched-settings-{name}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("test directory must be created");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn append(path: &Path, contents: &str) {
        let mut file = OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
    }

    #[test]
    fn pretty_toml_roundtrip_preserves_every_field() {
        let mut config = ControllerConfig {
            controller_mode: ControllerMode::Auto,
            sample_interval_ms: 2_500,
            minimum_process_utilization_bps: 725,
            all_user_processes: true,
            default_rule_mode: RuleMode::Performance,
            ..ControllerConfig::default()
        };
        config.policy.overload_threshold_bps = 9_000;
        config.policy.minimum_improvement_bps = 1_250;
        config.policy.stability_samples = 5;
        config.policy.minimum_residency_ms = 20_000;
        config.policy.cooldown_ms = 45_000;
        config.policy.max_mutations_per_evaluation = 2;
        config.rules.push(ProcessRule {
            image: "Game.exe".to_owned(),
            mode: RuleMode::Strict,
            group: Some(1),
            llc: Some(2),
        });

        let rendered = render_config(&config).unwrap();
        assert_eq!(ControllerConfig::from_toml(&rendered).unwrap(), config);
    }

    #[test]
    fn restore_defaults_resets_an_edited_document() {
        let mut edited = ControllerConfig {
            controller_mode: ControllerMode::Auto,
            ..ControllerConfig::default()
        };
        edited.rules.push(ProcessRule {
            image: "app.exe".to_owned(),
            mode: RuleMode::Auto,
            group: None,
            llc: None,
        });

        assert_ne!(edited, restore_defaults().unwrap());
        edited = restore_defaults().unwrap();
        let shipped = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/winsched.default.toml"),
        )
        .unwrap();
        let shipped = ControllerConfig::from_toml(&shipped).unwrap();
        assert_eq!(edited, shipped);
        assert_eq!(edited.controller_mode, ControllerMode::Auto);
        assert!(edited.all_user_processes);
    }

    #[test]
    fn validation_failure_does_not_replace_existing_file() {
        let directory = TestDirectory::new("validation");
        let path = directory.0.join("winsched.toml");
        fs::write(&path, "original contents").unwrap();
        let invalid = ControllerConfig {
            sample_interval_ms: 999,
            ..ControllerConfig::default()
        };

        assert!(save_config_atomic(&path, &invalid).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "original contents");
    }

    #[test]
    fn atomic_save_replaces_file_with_valid_pretty_toml() {
        let directory = TestDirectory::new("atomic");
        let path = directory.0.join("nested").join("winsched.toml");
        let config = ControllerConfig {
            controller_mode: ControllerMode::Auto,
            ..ControllerConfig::default()
        };

        let saved = save_config_atomic(&path, &config).unwrap();
        let contents = fs::read_to_string(&path).unwrap();

        assert_eq!(saved, config);
        assert!(contents.contains("controller_mode = \"auto\""));
        assert_eq!(load_config(&path).unwrap(), config);
    }

    #[test]
    fn tray_autostart_enable_copies_installed_shortcut_atomically() {
        let directory = TestDirectory::new("autostart-enable");
        let source = directory.0.join("Programs/WinSched/WinSched.lnk");
        let startup = directory.0.join("Programs/Startup/WinSched Tray.lnk");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, b"shortcut payload").unwrap();

        set_tray_autostart(&source, &startup, true).unwrap();

        assert!(tray_autostart_enabled(&startup));
        assert_eq!(fs::read(startup).unwrap(), b"shortcut payload");
    }

    #[test]
    fn tray_autostart_disable_is_idempotent_and_preserves_source() {
        let directory = TestDirectory::new("autostart-disable");
        let source = directory.0.join("Programs/WinSched/WinSched.lnk");
        let startup = directory.0.join("Programs/Startup/WinSched Tray.lnk");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::create_dir_all(startup.parent().unwrap()).unwrap();
        fs::write(&source, b"source").unwrap();
        fs::write(&startup, b"startup").unwrap();

        set_tray_autostart(&source, &startup, false).unwrap();
        set_tray_autostart(&source, &startup, false).unwrap();

        assert!(!tray_autostart_enabled(&startup));
        assert_eq!(fs::read(source).unwrap(), b"source");
    }

    #[test]
    fn missing_installed_shortcut_does_not_replace_existing_startup_entry() {
        let directory = TestDirectory::new("autostart-missing-source");
        let source = directory.0.join("Programs/WinSched/WinSched.lnk");
        let startup = directory.0.join("Programs/Startup/WinSched Tray.lnk");
        fs::create_dir_all(startup.parent().unwrap()).unwrap();
        fs::write(&startup, b"existing startup shortcut").unwrap();

        assert!(set_tray_autostart(&source, &startup, true).is_err());
        assert_eq!(fs::read(startup).unwrap(), b"existing startup shortcut");
    }

    #[test]
    fn settings_paths_target_the_common_start_menu_and_startup_folders() {
        let paths = SettingsPaths::under_program_data(Path::new("ProgramData"));

        assert!(paths.tray_shortcut.ends_with(Path::new(
            "Microsoft/Windows/Start Menu/Programs/WinSched/WinSched.lnk"
        )));
        assert!(paths.tray_startup_shortcut.ends_with(Path::new(
            "Microsoft/Windows/Start Menu/Programs/Startup/WinSched Tray.lnk"
        )));
        assert!(paths.log.ends_with(Path::new("WinSched/winsched.log")));
    }

    #[test]
    fn reload_event_is_detected_only_after_the_captured_offset() {
        let directory = TestDirectory::new("log-offset");
        let log = directory.0.join("winsched.log");
        fs::write(
            &log,
            "{\"event\":\"config_rejected_fail_closed\",\"timestamp_ms\":10}\n",
        )
        .unwrap();
        let mut cursor = EventLogCursor::capture(&log).unwrap();
        let baseline_length = fs::metadata(&log).unwrap().len();
        assert_eq!(cursor.offset(), baseline_length);

        append(
            &log,
            "{\"event\":\"sample\"}\n{\"event\":\"config_reloaded\",\"timestamp_ms\":20}\n",
        );

        assert_eq!(
            read_config_reload_event(&log, &mut cursor).unwrap(),
            Some(ConfigReloadLogEvent::Reloaded {
                timestamp_ms: Some(20)
            })
        );
        assert_eq!(read_config_reload_event(&log, &mut cursor).unwrap(), None);
    }

    #[test]
    fn rejected_event_preserves_durable_error_detail() {
        let directory = TestDirectory::new("log-rejected");
        let log = directory.0.join("winsched.log");
        fs::write(&log, "{\"event\":\"baseline\"}\n").unwrap();
        let mut cursor = EventLogCursor::capture(&log).unwrap();
        append(
            &log,
            "{\"event\":\"config_rejected_fail_closed\",\"timestamp_ms\":30,\"error\":\"bad config\"}\n",
        );

        assert_eq!(
            read_config_reload_event(&log, &mut cursor).unwrap(),
            Some(ConfigReloadLogEvent::Rejected {
                timestamp_ms: Some(30),
                error: Some("bad config".to_owned()),
            })
        );
    }

    #[test]
    fn truncation_and_regrowth_restart_from_the_beginning() {
        let directory = TestDirectory::new("log-truncated");
        let log = directory.0.join("winsched.log");
        fs::write(&log, format!("{}\n", "old".repeat(100))).unwrap();
        let mut cursor = EventLogCursor::capture(&log).unwrap();
        fs::write(
            &log,
            format!(
                "{}\n{{\"event\":\"config_reloaded\",\"timestamp_ms\":40}}\n",
                "new".repeat(200)
            ),
        )
        .unwrap();

        assert_eq!(
            read_config_reload_event(&log, &mut cursor).unwrap(),
            Some(ConfigReloadLogEvent::Reloaded {
                timestamp_ms: Some(40)
            })
        );
    }

    #[test]
    fn rotation_to_a_longer_file_restarts_from_the_beginning() {
        let directory = TestDirectory::new("log-rotated");
        let log = directory.0.join("winsched.log");
        let rotated = directory.0.join("winsched.log.1");
        fs::write(&log, "{\"event\":\"old baseline\"}\n").unwrap();
        let mut cursor = EventLogCursor::capture(&log).unwrap();
        fs::rename(&log, rotated).unwrap();
        fs::write(
            &log,
            format!(
                "{}\n{{\"event\":\"config_reloaded\",\"timestamp_ms\":50}}\n",
                "rotated".repeat(100)
            ),
        )
        .unwrap();

        assert_eq!(
            read_config_reload_event(&log, &mut cursor).unwrap(),
            Some(ConfigReloadLogEvent::Reloaded {
                timestamp_ms: Some(50)
            })
        );
    }
}
