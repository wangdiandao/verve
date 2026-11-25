use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::schema::validate_safe_file_name;
use crate::{ConfigError, LocalMachineConfig, StateStore, SyncableConfig, atomic_write_str};

/// Coordinates the on-disk layout of Verve's dual-domain configuration,
/// profile/override files, compiled runtime artifacts, and SQLite metadata.
#[derive(Debug, Clone)]
pub struct ConfigManager {
    root: PathBuf,
}

impl ConfigManager {
    /// Initializes a [`ConfigManager`] rooted at `root`, ensuring required
    /// subdirectories (`profiles/`, `overrides/`, `runtime/`) exist.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        let root = root.into();
        let manager = Self { root };
        manager.ensure_layout()?;
        Ok(manager)
    }

    /// Resolves the platform-native default application data directory via `directories`.
    pub fn default_data_dir() -> Option<PathBuf> {
        directories::ProjectDirs::from("io", "verve", "verve")
            .map(|dirs| dirs.data_local_dir().to_path_buf())
    }

    fn ensure_layout(&self) -> Result<(), ConfigError> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir_all(self.profiles_dir())?;
        std::fs::create_dir_all(self.overrides_dir())?;
        std::fs::create_dir_all(self.runtime_dir())?;
        Ok(())
    }

    /// Returns the root application data directory.
    pub fn root_dir(&self) -> &Path {
        &self.root
    }

    /// Returns the path to `settings.toml`.
    pub fn settings_path(&self) -> PathBuf {
        self.root.join("settings.toml")
    }

    /// Returns the path to `local.toml`.
    pub fn local_path(&self) -> PathBuf {
        self.root.join("local.toml")
    }

    /// Returns the path to `state.db`.
    pub fn state_db_path(&self) -> PathBuf {
        self.root.join("state.db")
    }

    /// Returns the `profiles/` directory path.
    pub fn profiles_dir(&self) -> PathBuf {
        self.root.join("profiles")
    }

    /// Returns the `overrides/` directory path.
    pub fn overrides_dir(&self) -> PathBuf {
        self.root.join("overrides")
    }

    /// Returns the `runtime/` directory path.
    pub fn runtime_dir(&self) -> PathBuf {
        self.root.join("runtime")
    }

    /// Resolves a validated relative profile file name inside `profiles/`.
    pub fn profile_path(&self, file_name: &str) -> Result<PathBuf, ConfigError> {
        validate_safe_file_name("profile_file_name", file_name)?;
        Ok(self.profiles_dir().join(file_name))
    }

    /// Resolves a validated relative override file name inside `overrides/`.
    pub fn override_path(&self, file_name: &str) -> Result<PathBuf, ConfigError> {
        validate_safe_file_name("override_file_name", file_name)?;
        Ok(self.overrides_dir().join(file_name))
    }

    /// Loads `settings.toml`, migrating older schemas or returning defaults if absent.
    pub fn load_syncable(&self) -> Result<SyncableConfig, ConfigError> {
        match std::fs::read_to_string(self.settings_path()) {
            Ok(raw) => SyncableConfig::from_toml_str(&raw),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(SyncableConfig::default()),
            Err(err) => Err(ConfigError::Io(err)),
        }
    }

    /// Atomically persists `config` to `settings.toml`.
    pub fn save_syncable(&self, config: &SyncableConfig) -> Result<(), ConfigError> {
        let toml = config.to_toml_string()?;
        atomic_write_str(&self.settings_path(), &toml)
    }

    /// Loads `local.toml`, migrating older schemas or returning defaults if absent.
    pub fn load_local(&self) -> Result<LocalMachineConfig, ConfigError> {
        match std::fs::read_to_string(self.local_path()) {
            Ok(raw) => LocalMachineConfig::from_toml_str(&raw),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(LocalMachineConfig::default()),
            Err(err) => Err(ConfigError::Io(err)),
        }
    }

    /// Atomically persists `config` to `local.toml`.
    pub fn save_local(&self, config: &LocalMachineConfig) -> Result<(), ConfigError> {
        let toml = config.to_toml_string()?;
        atomic_write_str(&self.local_path(), &toml)
    }

    /// Reads raw YAML content for a profile stored in `profiles/{file_name}`.
    pub fn read_profile_yaml(&self, file_name: &str) -> Result<String, ConfigError> {
        let path = self.profile_path(file_name)?;
        Ok(std::fs::read_to_string(path)?)
    }

    /// Atomically writes raw YAML content for a profile into `profiles/{file_name}`.
    pub fn write_profile_yaml(&self, file_name: &str, yaml: &str) -> Result<(), ConfigError> {
        let path = self.profile_path(file_name)?;
        atomic_write_str(&path, yaml)
    }

    /// Reads an override (Merge YAML or JS script) file from `overrides/{file_name}`.
    pub fn read_override_file(&self, file_name: &str) -> Result<String, ConfigError> {
        let path = self.override_path(file_name)?;
        Ok(std::fs::read_to_string(path)?)
    }

    /// Atomically writes an override (Merge YAML or JS script) file into `overrides/{file_name}`.
    pub fn write_override_file(&self, file_name: &str, content: &str) -> Result<(), ConfigError> {
        let path = self.override_path(file_name)?;
        atomic_write_str(&path, content)
    }

    /// Opens the SQLite WAL runtime metadata store at `state.db`.
    pub fn open_state_store(&self) -> Result<StateStore, ConfigError> {
        StateStore::open(&self.state_db_path())
    }
}
