use std::io::Write;
use std::path::Path;

use crate::ConfigError;

/// Atomically writes `bytes` to `path` using a same-directory temporary file,
/// `sync_all`, and atomic rename replacement.
pub fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(parent)?;

    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|err| ConfigError::Persist {
        path: path.to_path_buf(),
        source: err.error,
    })?;
    Ok(())
}

/// Atomically writes UTF-8 `content` to `path`.
pub fn atomic_write_str(path: &Path, content: &str) -> Result<(), ConfigError> {
    atomic_write_bytes(path, content.as_bytes())
}
