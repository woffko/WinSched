//! Validated, fail-closed controller configuration.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use winsched_core::{
    LlcDomainKey,
    adaptive::{EnforcementMode, PlacementMode, PolicyConfig},
};

pub const LEGACY_CONFIG_SCHEMA_VERSION: u32 = 1;
pub const CONFIG_SCHEMA_VERSION: u32 = 2;
pub const MIN_LOG_FILE_SIZE_MIB: u16 = 1;
pub const MAX_LOG_FILE_SIZE_MIB: u16 = 100;
pub const MAX_RETAINED_LOG_ARCHIVES: u8 = 10;

/// Global mutation gate for the controller.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerMode {
    Off,
    #[default]
    Observe,
    Auto,
}

/// Placement behavior requested by one exact executable-name rule.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleMode {
    Off,
    Sticky,
    #[default]
    Auto,
    Performance,
    Efficiency,
    Strict,
}

/// One exact, case-insensitive executable-name rule.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRule {
    pub image: String,
    #[serde(default)]
    pub mode: RuleMode,
    #[serde(default)]
    pub group: Option<u16>,
    #[serde(default)]
    pub llc: Option<u8>,
}

/// Bounded diagnostic event logging policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    pub enabled: bool,
    pub max_file_size_mib: u16,
    pub retained_archives: u8,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_file_size_mib: 10,
            retained_archives: 1,
        }
    }
}

impl LoggingConfig {
    /// Maximum active file length in bytes.
    #[must_use]
    pub fn max_file_size_bytes(self) -> u64 {
        u64::from(self.max_file_size_mib) * 1024 * 1024
    }
}

/// Full service configuration.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ControllerConfig {
    pub schema_version: u32,
    pub controller_mode: ControllerMode,
    pub sample_interval_ms: u64,
    pub minimum_process_utilization_bps: u16,
    pub all_user_processes: bool,
    pub default_rule_mode: RuleMode,
    pub logging: LoggingConfig,
    pub policy: PolicyConfig,
    pub rules: Vec<ProcessRule>,
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            controller_mode: ControllerMode::Observe,
            sample_interval_ms: 1_000,
            minimum_process_utilization_bps: 500,
            all_user_processes: false,
            default_rule_mode: RuleMode::Auto,
            logging: LoggingConfig::default(),
            policy: PolicyConfig::default(),
            rules: Vec::new(),
        }
    }
}

/// A rule resolved into independent placement and enforcement controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedRule {
    pub placement: PlacementMode,
    pub enforcement: EnforcementMode,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to parse TOML configuration: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("unsupported schema_version {0}; expected {CONFIG_SCHEMA_VERSION}")]
    SchemaVersion(u32),
    #[error("sample_interval_ms must be between 1000 and 60000")]
    SampleInterval,
    #[error("minimum_process_utilization_bps must be <= 10000")]
    ProcessUtilization,
    #[error("default_rule_mode cannot be strict because no default group and LLC are defined")]
    StrictDefaultUnsupported,
    #[error("logging.max_file_size_mib must be between 1 and 100")]
    LogFileSize,
    #[error("logging.retained_archives must be <= 10")]
    LogArchives,
    #[error("invalid policy: {0}")]
    Policy(#[from] winsched_core::adaptive::AdaptiveError),
    #[error("rule image must be an executable name without path separators")]
    InvalidImage,
    #[error("duplicate case-insensitive rule for image '{0}'")]
    DuplicateImage(String),
    #[error("strict rule for '{0}' requires both group and llc")]
    StrictDomainRequired(String),
    #[error("non-strict rule for '{0}' must not set group or llc")]
    UnexpectedDomain(String),
}

impl ControllerConfig {
    /// Stable within one `WinSched` build and used to match a live service receipt to Settings.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }

    /// Parses and validates a fail-closed TOML document.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for syntax errors, unknown fields, unsupported
    /// schema versions, unsafe sampling intervals, or inconsistent rules.
    pub fn from_toml(value: &str) -> Result<Self, ConfigError> {
        let mut config = toml::from_str::<Self>(value)?;
        if config.schema_version == LEGACY_CONFIG_SCHEMA_VERSION {
            config.schema_version = CONFIG_SCHEMA_VERSION;
        }
        config.validate()
    }

    /// Validates an already-deserialized configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when any global or per-rule invariant fails.
    pub fn validate(mut self) -> Result<Self, ConfigError> {
        if self.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(ConfigError::SchemaVersion(self.schema_version));
        }
        if !(1_000..=60_000).contains(&self.sample_interval_ms) {
            return Err(ConfigError::SampleInterval);
        }
        if self.minimum_process_utilization_bps > 10_000 {
            return Err(ConfigError::ProcessUtilization);
        }
        if self.default_rule_mode == RuleMode::Strict {
            return Err(ConfigError::StrictDefaultUnsupported);
        }
        if !(MIN_LOG_FILE_SIZE_MIB..=MAX_LOG_FILE_SIZE_MIB)
            .contains(&self.logging.max_file_size_mib)
        {
            return Err(ConfigError::LogFileSize);
        }
        if self.logging.retained_archives > MAX_RETAINED_LOG_ARCHIVES {
            return Err(ConfigError::LogArchives);
        }
        self.policy = self.policy.validate()?;

        let mut images = BTreeSet::new();
        for rule in &mut self.rules {
            rule.image = rule.image.trim().to_owned();
            if rule.image.is_empty() || rule.image.contains(['/', '\\']) {
                return Err(ConfigError::InvalidImage);
            }
            if !images.insert(rule.image.to_lowercase()) {
                return Err(ConfigError::DuplicateImage(rule.image.clone()));
            }
            match (rule.mode, rule.group, rule.llc) {
                (RuleMode::Strict, Some(_), Some(_)) => {}
                (RuleMode::Strict, _, _) => {
                    return Err(ConfigError::StrictDomainRequired(rule.image.clone()));
                }
                (_, None, None) => {}
                _ => return Err(ConfigError::UnexpectedDomain(rule.image.clone())),
            }
        }
        Ok(self)
    }

    /// Resolves one executable name. No match means the process is out of scope.
    #[must_use]
    pub fn resolve(&self, image_name: &str) -> Option<ResolvedRule> {
        if self.controller_mode == ControllerMode::Off {
            return None;
        }
        let rule = self
            .rules
            .iter()
            .find(|rule| rule.image.eq_ignore_ascii_case(image_name));
        if rule.is_none() && !self.all_user_processes {
            return None;
        }
        let mode = rule.map_or(self.default_rule_mode, |rule| rule.mode);
        let placement = match mode {
            RuleMode::Off => PlacementMode::Off,
            RuleMode::Sticky => PlacementMode::Sticky,
            RuleMode::Auto => PlacementMode::Auto,
            RuleMode::Performance => PlacementMode::Performance,
            RuleMode::Efficiency => PlacementMode::Efficiency,
            RuleMode::Strict => {
                let rule = rule?;
                let (Some(group), Some(last_level_cache_index)) = (rule.group, rule.llc) else {
                    return None;
                };
                PlacementMode::Strict(LlcDomainKey {
                    group,
                    last_level_cache_index,
                })
            }
        };
        Some(ResolvedRule {
            placement,
            enforcement: match self.controller_mode {
                ControllerMode::Observe => EnforcementMode::Observe,
                ControllerMode::Auto => EnforcementMode::Apply,
                ControllerMode::Off => unreachable!("off returned before rule resolution"),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_observe_only_and_scope_nothing() {
        let config = ControllerConfig::from_toml("schema_version = 1").unwrap();
        assert_eq!(config.controller_mode, ControllerMode::Observe);
        assert_eq!(config.resolve("app.exe"), None);
    }

    #[test]
    fn observe_preserves_requested_placement_without_enforcement() {
        let config = ControllerConfig::from_toml(
            r#"
schema_version = 1
controller_mode = "observe"

[[rules]]
image = "Game.exe"
mode = "performance"
"#,
        )
        .unwrap();
        assert_eq!(
            config.resolve("game.EXE"),
            Some(ResolvedRule {
                placement: PlacementMode::Performance,
                enforcement: EnforcementMode::Observe,
            })
        );
    }

    #[test]
    fn auto_enables_enforcement() {
        let config = ControllerConfig::from_toml(
            r#"
schema_version = 1
controller_mode = "auto"
all_user_processes = true
default_rule_mode = "sticky"
"#,
        )
        .unwrap();
        assert_eq!(
            config.resolve("app.exe"),
            Some(ResolvedRule {
                placement: PlacementMode::Sticky,
                enforcement: EnforcementMode::Apply,
            })
        );
    }

    #[test]
    fn strict_rule_requires_complete_domain() {
        let error = ControllerConfig::from_toml(
            r#"
schema_version = 1
[[rules]]
image = "app.exe"
mode = "strict"
group = 0
"#,
        )
        .unwrap_err();
        assert!(matches!(error, ConfigError::StrictDomainRequired(_)));
    }

    #[test]
    fn duplicate_image_rules_are_rejected_case_insensitively() {
        let error = ControllerConfig::from_toml(
            r#"
schema_version = 1
[[rules]]
image = "App.exe"
[[rules]]
image = "app.EXE"
"#,
        )
        .unwrap_err();
        assert!(matches!(error, ConfigError::DuplicateImage(_)));
    }

    #[test]
    fn partial_policy_table_uses_safe_defaults() {
        let config = ControllerConfig::from_toml(
            r"
schema_version = 1
[policy]
stability_samples = 5
",
        )
        .unwrap();
        assert_eq!(config.policy.stability_samples, 5);
        assert_eq!(config.policy.cooldown_ms, 30_000);
    }

    #[test]
    fn legacy_config_uses_current_logging_defaults() {
        let config = ControllerConfig::from_toml("schema_version = 1").unwrap();
        assert_eq!(config.schema_version, CONFIG_SCHEMA_VERSION);
        assert_eq!(config.logging, LoggingConfig::default());
        assert_eq!(config.logging.max_file_size_bytes(), 10 * 1024 * 1024);
    }

    #[test]
    fn fingerprint_matches_normalized_legacy_config_and_changes_with_content() {
        let legacy = ControllerConfig::from_toml("schema_version = 1").unwrap();
        let current = ControllerConfig::from_toml("schema_version = 2").unwrap();
        assert_eq!(legacy.fingerprint(), current.fingerprint());

        let mut changed = current;
        changed.logging.enabled = false;
        assert_ne!(legacy.fingerprint(), changed.fingerprint());
    }

    #[test]
    fn partial_logging_table_preserves_bounded_defaults() {
        let config = ControllerConfig::from_toml(
            r"
schema_version = 1
[logging]
enabled = false
",
        )
        .unwrap();
        assert!(!config.logging.enabled);
        assert_eq!(config.logging.max_file_size_mib, 10);
        assert_eq!(config.logging.retained_archives, 1);
    }

    #[test]
    fn logging_bounds_apply_even_when_logging_is_disabled() {
        for document in [
            "schema_version = 1\n[logging]\nenabled = false\nmax_file_size_mib = 0\n",
            "schema_version = 1\n[logging]\nenabled = false\nmax_file_size_mib = 101\n",
            "schema_version = 1\n[logging]\nenabled = false\nretained_archives = 11\n",
        ] {
            assert!(ControllerConfig::from_toml(document).is_err());
        }
    }

    #[test]
    fn unsupported_config_schema_is_rejected() {
        for schema in [0, 3] {
            let error =
                ControllerConfig::from_toml(&format!("schema_version = {schema}")).unwrap_err();
            assert!(matches!(error, ConfigError::SchemaVersion(value) if value == schema));
        }
    }

    #[test]
    fn unknown_fields_fail_closed() {
        assert!(ControllerConfig::from_toml("schema_version = 1\nmagic = true").is_err());
    }

    #[test]
    fn process_activity_threshold_is_bounded() {
        let error = ControllerConfig::from_toml(
            r"
schema_version = 1
minimum_process_utilization_bps = 10001
",
        )
        .unwrap_err();
        assert!(matches!(error, ConfigError::ProcessUtilization));
    }

    #[test]
    fn strict_mode_is_rejected_as_a_default_without_a_domain() {
        let error = ControllerConfig::from_toml(
            r#"
schema_version = 1
default_rule_mode = "strict"
"#,
        )
        .unwrap_err();
        assert!(matches!(error, ConfigError::StrictDefaultUnsupported));
    }
}
