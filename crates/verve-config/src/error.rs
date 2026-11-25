use std::path::PathBuf;

use thiserror::Error;

/// Domain errors produced by configuration persistence, migration, and metadata operations.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Filesystem I/O operation failed.
    #[error("configuration I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Atomic rename persistence failed after writing and syncing the temporary file.
    #[error("failed to atomically persist configuration file `{path}`: {source}")]
    Persist {
        /// Target destination path.
        path: PathBuf,
        /// Underlying OS rename error.
        #[source]
        source: std::io::Error,
    },

    /// Parsing TOML configuration content failed.
    #[error("failed to parse TOML configuration: {0}")]
    TomlDeserialize(#[from] toml::de::Error),

    /// Serializing configuration into TOML failed.
    #[error("failed to serialize TOML configuration: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    /// SQLite metadata operation failed.
    #[error("SQLite metadata store error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Configuration file declares a schema version newer than this binary supports.
    #[error("unsupported schema_version {found} (maximum supported is {supported})")]
    UnsupportedSchemaVersion {
        /// Schema version read from disk.
        found: u32,
        /// Highest schema version understood by this build.
        supported: u32,
    },

    /// A configuration field or parameter failed validation.
    #[error("invalid configuration field `{field}` (`{value}`): {reason}")]
    InvalidField {
        /// Name of the invalid field.
        field: &'static str,
        /// Offending value representation.
        value: String,
        /// Human-readable validation reason.
        reason: String,
    },
}
