//! Validated, atomic persistence for the `WinSched` settings application.

#![forbid(unsafe_code)]

use std::fs;
#[cfg(not(windows))]
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(not(windows))]
use atomic_write_file::AtomicWriteFile;
use thiserror::Error;
use winsched_config::ControllerConfig;
use winsched_control::{CONFIG_FILE_NAME, INSTALL_DIRECTORY_NAME, LOG_FILE_NAME, STATUS_FILE_NAME};

const PRODUCT_DEFAULT_CONFIG: &str = include_str!("../../../config/winsched.default.toml");

/// Allows one complete old or new service interval plus a bounded receipt margin.
#[must_use]
pub fn config_reload_wait_ms(previous_interval_ms: u64, updated_interval_ms: u64) -> u64 {
    previous_interval_ms
        .max(updated_interval_ms)
        .saturating_add(5_000)
}

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

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    #[cfg(windows)]
    {
        winsched::platform::atomic_replace_file(path, bytes)
    }
    #[cfg(not(windows))]
    {
        let mut output = AtomicWriteFile::options().open(path)?;
        output.write_all(bytes)?;
        output.commit()
    }
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
    write_atomic(path, encoded.as_bytes())?;
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
    write_atomic(startup_shortcut, &shortcut)?;
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
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use winsched_config::{ControllerMode, ProcessRule, RuleMode, WorkloadProfile};

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
        config.logging.enabled = false;
        config.logging.max_file_size_mib = 77;
        config.logging.retained_archives = 0;
        config.rules.push(ProcessRule {
            image: "Game.exe".to_owned(),
            mode: RuleMode::Strict,
            profile: WorkloadProfile::Memory,
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
            profile: WorkloadProfile::Balanced,
            group: None,
            llc: None,
        });

        assert_ne!(edited, restore_defaults().unwrap());
        edited = restore_defaults().unwrap();
        let shipped = ControllerConfig::from_toml(PRODUCT_DEFAULT_CONFIG).unwrap();
        assert_eq!(edited, shipped);
        assert_eq!(edited.controller_mode, ControllerMode::Auto);
        assert!(edited.all_user_processes);
        assert!(edited.logging.enabled);
        assert_eq!(edited.logging.max_file_size_mib, 10);
        assert_eq!(edited.logging.retained_archives, 1);
        assert!(!edited.background_efficiency.enabled);
        assert!(!edited.background_efficiency.eco_qos_enabled);
        assert!(!edited.background_efficiency.memory_priority_enabled);
        assert!(edited.background_efficiency.protect_visible);
        assert!(edited.responsiveness.enabled);
        assert_eq!(edited.responsiveness.system_reserve_percent, 10);
        assert!(!edited.responsiveness.memory.use_smt);
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
    fn first_save_migrates_schema_one_without_losing_existing_values() {
        let directory = TestDirectory::new("schema-one-migration");
        let path = directory.0.join("winsched.toml");
        fs::write(
            &path,
            "schema_version = 1\nsample_interval_ms = 2500\nminimum_process_utilization_bps = 777\n",
        )
        .unwrap();

        let loaded = load_config(&path).unwrap();
        assert_eq!(loaded.schema_version, 4);
        assert_eq!(loaded.sample_interval_ms, 2_500);
        assert_eq!(loaded.minimum_process_utilization_bps, 777);
        assert!(loaded.logging.enabled);
        assert_eq!(loaded.logging.max_file_size_mib, 10);
        assert_eq!(loaded.logging.retained_archives, 1);
        assert!(!loaded.responsiveness.enabled);
        assert!(!loaded.background_efficiency.enabled);

        save_config_atomic(&path, &loaded).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("schema_version = 4"));
        assert!(saved.contains("minimum_process_utilization_bps = 777"));
        assert!(saved.contains("[logging]"));
        assert!(saved.contains("[responsiveness]"));
        assert!(saved.contains("[background_efficiency]"));
        assert_eq!(load_config(&path).unwrap(), loaded);
    }

    #[test]
    fn reload_wait_covers_the_old_interval_when_the_new_interval_is_shorter() {
        assert_eq!(config_reload_wait_ms(60_000, 1_000), 65_000);
        assert_eq!(config_reload_wait_ms(1_000, 60_000), 65_000);
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
}
