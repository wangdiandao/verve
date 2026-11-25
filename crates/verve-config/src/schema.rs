use serde::de::DeserializeOwned;

use crate::ConfigError;

/// Current schema version for `settings.toml` and `local.toml`.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Parses a TOML document, validates its `schema_version`, runs deterministic
/// forward migrations when needed, and deserializes into `T`.
pub(crate) fn parse_and_migrate_toml<T, F>(raw: &str, migrate_v0: F) -> Result<T, ConfigError>
where
    T: DeserializeOwned,
    F: FnOnce(&mut toml::Table) -> Result<(), ConfigError>,
{
    let mut table: toml::Table = toml::from_str(raw)?;
    let version = extract_schema_version(&table)?;

    if version > CURRENT_SCHEMA_VERSION {
        return Err(ConfigError::UnsupportedSchemaVersion {
            found: version,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }

    if version == 0 {
        migrate_v0(&mut table)?;
        table.insert(
            "schema_version".into(),
            toml::Value::Integer(i64::from(CURRENT_SCHEMA_VERSION)),
        );
    }

    let decoded = toml::Value::Table(table).try_into::<T>()?;
    Ok(decoded)
}

fn extract_schema_version(table: &toml::Table) -> Result<u32, ConfigError> {
    match table.get("schema_version") {
        None => Ok(0),
        Some(toml::Value::Integer(raw)) => {
            if *raw < 0 {
                return Err(ConfigError::InvalidField {
                    field: "schema_version",
                    value: raw.to_string(),
                    reason: "schema_version must be non-negative".into(),
                });
            }
            u32::try_from(*raw).map_err(|_| ConfigError::InvalidField {
                field: "schema_version",
                value: raw.to_string(),
                reason: "schema_version exceeds u32 range".into(),
            })
        }
        Some(other) => Err(ConfigError::InvalidField {
            field: "schema_version",
            value: other.to_string(),
            reason: "schema_version must be an integer".into(),
        }),
    }
}

/// Validates that `name` is a single, non-empty relative file name without path separators.
pub(crate) fn validate_safe_file_name(field: &'static str, name: &str) -> Result<(), ConfigError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed != name {
        return Err(ConfigError::InvalidField {
            field,
            value: name.to_owned(),
            reason: "file name must be non-empty without leading or trailing whitespace".into(),
        });
    }
    if name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(ConfigError::InvalidField {
            field,
            value: name.to_owned(),
            reason: "file name must not contain path separators or traversal segments".into(),
        });
    }
    let mut components = std::path::Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(_)), None) => Ok(()),
        _ => Err(ConfigError::InvalidField {
            field,
            value: name.to_owned(),
            reason: "file name must be a single normal path component".into(),
        }),
    }
}
