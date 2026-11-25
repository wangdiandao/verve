use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ConfigError;
use crate::schema::{CURRENT_SCHEMA_VERSION, parse_and_migrate_toml};

/// Machine-local configuration domain persisted to `local.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalMachineConfig {
    /// Configuration schema version for deterministic forward migrations.
    pub schema_version: u32,
    /// Optional explicit path override to the Mihomo kernel executable.
    pub mihomo_binary_path: Option<PathBuf>,
    /// Whether privileged `verve-service` supervision is enabled.
    pub service_mode_enabled: bool,
    /// Optional custom TUN network interface name.
    pub tun_interface_name: Option<String>,
    /// Whether Verve should launch automatically on user login.
    pub auto_launch: bool,
    /// Remembered main window width in logical pixels.
    pub window_width: Option<u32>,
    /// Remembered main window height in logical pixels.
    pub window_height: Option<u32>,
}

impl Default for LocalMachineConfig {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            mihomo_binary_path: None,
            service_mode_enabled: false,
            tun_interface_name: None,
            auto_launch: false,
            window_width: None,
            window_height: None,
        }
    }
}

impl LocalMachineConfig {
    /// Parses `local.toml`, migrating older schema versions forward if necessary.
    pub fn from_toml_str(raw: &str) -> Result<Self, ConfigError> {
        let mut config: Self = parse_and_migrate_toml(raw, migrate_local_v0)?;
        config.normalize_and_validate()?;
        Ok(config)
    }

    /// Validates and serializes this configuration into a TOML string.
    pub fn to_toml_string(&self) -> Result<String, ConfigError> {
        let mut normalized = self.clone();
        normalized.normalize_and_validate()?;
        Ok(toml::to_string_pretty(&normalized)?)
    }

    /// Normalizes canonical fields and validates invariants.
    pub fn normalize_and_validate(&mut self) -> Result<(), ConfigError> {
        self.schema_version = CURRENT_SCHEMA_VERSION;
        if let Some(name) = &self.tun_interface_name {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                self.tun_interface_name = None;
            } else if trimmed != name {
                self.tun_interface_name = Some(trimmed.to_owned());
            }
        }
        Ok(())
    }
}

fn migrate_local_v0(_table: &mut toml::Table) -> Result<(), ConfigError> {
    // v0 -> v1: missing fields fall back to LocalMachineConfig::default() during deserialization,
    // and `schema_version` is upgraded deterministically to CURRENT_SCHEMA_VERSION.
    Ok(())
}
