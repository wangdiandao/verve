use serde::{Deserialize, Serialize};

use crate::ConfigError;
use crate::schema::{CURRENT_SCHEMA_VERSION, parse_and_migrate_toml, validate_safe_file_name};

/// Origin metadata for an imported or local proxy profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProfileSource {
    /// Profile authored or imported locally from a file.
    Local,
    /// Remote subscription URL with optional periodic refresh interval.
    Remote {
        /// Subscription source URL.
        url: String,
        /// Automatic update interval in minutes, if enabled.
        #[serde(default)]
        update_interval_mins: Option<u32>,
    },
}

/// Registered profile entry stored in [`SyncableConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileEntry {
    /// Stable unique profile identifier.
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Local or remote origin details.
    pub source: ProfileSource,
    /// Relative YAML file name inside `profiles/`.
    pub file_name: String,
    /// Optional per-profile declarative Merge YAML file name inside `overrides/`.
    #[serde(default)]
    pub merge_file_name: Option<String>,
    /// Optional per-profile JS script file name inside `overrides/`.
    #[serde(default)]
    pub script_file_name: Option<String>,
}

/// Syncable configuration domain persisted to `settings.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncableConfig {
    /// Configuration schema version for deterministic forward migrations.
    pub schema_version: u32,
    /// Active routing mode (`"rule"`, `"global"`, or `"direct"`).
    pub proxy_mode: String,
    /// Combined HTTP/SOCKS5 listening port (`mixed-port`).
    pub mixed_port: u16,
    /// Whether inbound proxy connections from LAN hosts are accepted.
    pub allow_lan: bool,
    /// Whether IPv6 resolution and forwarding are enabled.
    pub ipv6: bool,
    /// Kernel log severity (`"debug"`, `"info"`, `"warning"`, or `"error"`).
    pub log_level: String,
    /// Whether OS system proxy settings should be managed by Verve.
    pub system_proxy_enabled: bool,
    /// Host/CIDR patterns bypassed by the OS system proxy.
    pub system_proxy_bypass: Vec<String>,
    /// Whether TUN virtual interface traffic capture is enabled.
    pub tun_enabled: bool,
    /// Identifier of the currently selected active profile, if any.
    pub active_profile_id: Option<String>,
    /// Registered user profiles.
    pub profiles: Vec<ProfileEntry>,
    /// Whether global declarative Merge (`overrides/global.merge.yaml`) is enabled.
    pub global_merge_enabled: bool,
    /// Whether global JS override script (`overrides/global.script.js`) is enabled.
    pub global_script_enabled: bool,
    /// Declarative quick rules prepended ahead of profile rules.
    pub quick_rules: Vec<String>,
    /// UI color theme preference (`"system"`, `"light"`, or `"dark"`).
    pub theme_mode: String,
    /// Preferred UI locale tag (for example `"zh-CN"` or `"en-US"`).
    pub locale: String,
}

impl Default for SyncableConfig {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            proxy_mode: "rule".into(),
            mixed_port: 7897,
            allow_lan: false,
            ipv6: false,
            log_level: "info".into(),
            system_proxy_enabled: false,
            system_proxy_bypass: Vec::new(),
            tun_enabled: false,
            active_profile_id: None,
            profiles: Vec::new(),
            global_merge_enabled: false,
            global_script_enabled: false,
            quick_rules: Vec::new(),
            theme_mode: "system".into(),
            locale: "zh-CN".into(),
        }
    }
}

impl SyncableConfig {
    /// Parses `settings.toml`, migrating older schema versions forward if necessary.
    pub fn from_toml_str(raw: &str) -> Result<Self, ConfigError> {
        let mut config: Self = parse_and_migrate_toml(raw, migrate_syncable_v0)?;
        config.normalize_and_validate()?;
        Ok(config)
    }

    /// Validates and serializes this configuration into a TOML string.
    pub fn to_toml_string(&self) -> Result<String, ConfigError> {
        let mut normalized = self.clone();
        normalized.normalize_and_validate()?;
        Ok(toml::to_string_pretty(&normalized)?)
    }

    /// Normalizes canonical string fields and validates invariants.
    pub fn normalize_and_validate(&mut self) -> Result<(), ConfigError> {
        self.schema_version = CURRENT_SCHEMA_VERSION;
        self.proxy_mode = normalize_proxy_mode(&self.proxy_mode)?;
        self.log_level = normalize_log_level(&self.log_level)?;
        self.theme_mode = normalize_theme_mode(&self.theme_mode)?;

        let trimmed_locale = self.locale.trim();
        if trimmed_locale.is_empty() {
            return Err(ConfigError::InvalidField {
                field: "locale",
                value: self.locale.clone(),
                reason: "locale must not be empty".into(),
            });
        }
        self.locale = trimmed_locale.to_owned();

        for profile in &self.profiles {
            if profile.id.trim().is_empty() {
                return Err(ConfigError::InvalidField {
                    field: "profiles.id",
                    value: profile.id.clone(),
                    reason: "profile id must not be empty".into(),
                });
            }
            if profile.name.trim().is_empty() {
                return Err(ConfigError::InvalidField {
                    field: "profiles.name",
                    value: profile.name.clone(),
                    reason: "profile name must not be empty".into(),
                });
            }
            validate_safe_file_name("profiles.file_name", &profile.file_name)?;
            if let Some(merge_file) = &profile.merge_file_name {
                validate_safe_file_name("profiles.merge_file_name", merge_file)?;
            }
            if let Some(script_file) = &profile.script_file_name {
                validate_safe_file_name("profiles.script_file_name", script_file)?;
            }
            if let ProfileSource::Remote { url, .. } = &profile.source
                && url.trim().is_empty()
            {
                return Err(ConfigError::InvalidField {
                    field: "profiles.source.url",
                    value: url.clone(),
                    reason: "remote profile URL must not be empty".into(),
                });
            }
        }

        Ok(())
    }
}

fn migrate_syncable_v0(_table: &mut toml::Table) -> Result<(), ConfigError> {
    // v0 -> v1: missing fields fall back to SyncableConfig::default() during deserialization,
    // and `schema_version` is upgraded deterministically to CURRENT_SCHEMA_VERSION.
    Ok(())
}

fn normalize_proxy_mode(raw: &str) -> Result<String, ConfigError> {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "rule" | "global" | "direct" => Ok(normalized),
        _ => Err(ConfigError::InvalidField {
            field: "proxy_mode",
            value: raw.to_owned(),
            reason: "expected one of \"rule\", \"global\", or \"direct\"".into(),
        }),
    }
}

fn normalize_log_level(raw: &str) -> Result<String, ConfigError> {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "debug" | "info" | "warning" | "error" => Ok(normalized),
        "warn" => Ok("warning".into()),
        _ => Err(ConfigError::InvalidField {
            field: "log_level",
            value: raw.to_owned(),
            reason: "expected one of \"debug\", \"info\", \"warning\", or \"error\"".into(),
        }),
    }
}

fn normalize_theme_mode(raw: &str) -> Result<String, ConfigError> {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "system" | "light" | "dark" => Ok(normalized),
        _ => Err(ConfigError::InvalidField {
            field: "theme_mode",
            value: raw.to_owned(),
            reason: "expected one of \"system\", \"light\", or \"dark\"".into(),
        }),
    }
}
