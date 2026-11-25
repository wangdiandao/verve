//! System proxy takeover, atomic snapshot persistence, and crash self-healing.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::SysError;

/// File name of the atomic takeover marker stored inside the runtime directory.
const TAKEOVER_MARKER_FILE: &str = "sysproxy_takeover.json";

/// Serializable snapshot of host OS proxy configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemProxySnapshot {
    /// Whether the OS system proxy is enabled.
    pub enable: bool,
    /// Proxy hostname or IP address.
    pub host: String,
    /// Proxy listening port.
    pub port: u16,
    /// Comma- or semicolon-separated proxy bypass list.
    pub bypass: String,
}

/// Persisted crash-recovery marker written before applying proxy takeover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeoverMarker {
    /// Original host proxy configuration captured before takeover.
    pub original: SystemProxySnapshot,
    /// Proxy host applied during takeover.
    pub takeover_host: String,
    /// Proxy port applied during takeover.
    pub takeover_port: u16,
}

/// Platform abstraction for reading and applying OS system proxy settings.
pub trait SystemProxyBackend: Send + Sync {
    /// Reads the current host OS proxy configuration.
    fn get_current(&self) -> Result<SystemProxySnapshot, SysError>;

    /// Applies `snapshot` to the host OS proxy configuration.
    fn apply(&self, snapshot: &SystemProxySnapshot) -> Result<(), SysError>;
}

/// Production [`SystemProxyBackend`] backed by the `sysproxy` crate.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsSystemProxyBackend;

impl SystemProxyBackend for OsSystemProxyBackend {
    fn get_current(&self) -> Result<SystemProxySnapshot, SysError> {
        let current = sysproxy::Sysproxy::get_system_proxy()
            .map_err(|err| SysError::Proxy(err.to_string()))?;
        Ok(SystemProxySnapshot {
            enable: current.enable,
            host: current.host,
            port: current.port,
            bypass: current.bypass,
        })
    }

    fn apply(&self, snapshot: &SystemProxySnapshot) -> Result<(), SysError> {
        let target = sysproxy::Sysproxy {
            enable: snapshot.enable,
            host: snapshot.host.clone(),
            port: snapshot.port,
            bypass: snapshot.bypass.clone(),
        };
        target
            .set_system_proxy()
            .map_err(|err| SysError::Proxy(err.to_string()))
    }
}

/// In-memory [`SystemProxyBackend`] for deterministic unit and integration testing.
///
/// Using this backend guarantees tests never mutate the developer or CI runner's host OS proxy.
#[derive(Debug, Clone, Default)]
pub struct InMemorySystemProxyBackend {
    state: Arc<Mutex<SystemProxySnapshot>>,
}

impl InMemorySystemProxyBackend {
    /// Creates an in-memory backend seeded with `initial` proxy state.
    pub fn new(initial: SystemProxySnapshot) -> Self {
        Self {
            state: Arc::new(Mutex::new(initial)),
        }
    }

    /// Returns a cloned copy of the current in-memory proxy snapshot.
    pub fn snapshot(&self) -> SystemProxySnapshot {
        self.state
            .lock()
            .expect("in-memory system proxy mutex poisoned")
            .clone()
    }
}

impl SystemProxyBackend for InMemorySystemProxyBackend {
    fn get_current(&self) -> Result<SystemProxySnapshot, SysError> {
        Ok(self.snapshot())
    }

    fn apply(&self, snapshot: &SystemProxySnapshot) -> Result<(), SysError> {
        *self
            .state
            .lock()
            .expect("in-memory system proxy mutex poisoned") = snapshot.clone();
        Ok(())
    }
}

/// Manages system proxy takeover and crash self-healing via an atomic disk marker.
#[derive(Debug, Clone)]
pub struct SystemProxyManager<B: SystemProxyBackend = OsSystemProxyBackend> {
    backend: B,
    marker_path: PathBuf,
}

impl<B: SystemProxyBackend + Default> SystemProxyManager<B> {
    /// Creates a manager using `B::default()` and storing its marker in `runtime_dir`.
    pub fn new(runtime_dir: impl AsRef<Path>) -> Self {
        Self::with_backend(runtime_dir, B::default())
    }
}

impl<B: SystemProxyBackend> SystemProxyManager<B> {
    /// Creates a manager backed by `backend` and storing its marker in `runtime_dir`.
    pub fn with_backend(runtime_dir: impl AsRef<Path>, backend: B) -> Self {
        Self {
            backend,
            marker_path: runtime_dir.as_ref().join(TAKEOVER_MARKER_FILE),
        }
    }

    /// Returns the path to the crash-recovery takeover marker file.
    pub fn marker_path(&self) -> &Path {
        &self.marker_path
    }

    /// Returns a reference to the underlying proxy backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Enables system proxy takeover for `host:port` with `bypass` entries.
    ///
    /// Reads the current system proxy and atomically writes the takeover marker file
    /// *before* applying the new proxy configuration.
    pub fn enable(&self, host: &str, port: u16, bypass: &[String]) -> Result<(), SysError> {
        let marker = if self.marker_path.exists() {
            let mut existing = self.read_marker()?;
            existing.takeover_host = host.to_owned();
            existing.takeover_port = port;
            existing
        } else {
            TakeoverMarker {
                original: self.backend.get_current()?,
                takeover_host: host.to_owned(),
                takeover_port: port,
            }
        };

        self.write_marker_atomic(&marker)?;

        let bypass_joined = join_bypass_list(bypass);
        let desired = SystemProxySnapshot {
            enable: true,
            host: host.to_owned(),
            port,
            bypass: bypass_joined,
        };
        self.backend.apply(&desired)
    }

    /// Restores the pre-takeover system proxy snapshot and removes the marker file.
    pub fn disable(&self) -> Result<(), SysError> {
        if !self.marker_path.exists() {
            return Ok(());
        }

        let marker = self.read_marker()?;
        self.backend.apply(&marker.original)?;
        remove_file_if_exists(&self.marker_path)?;
        Ok(())
    }

    /// Checks for a stale takeover marker left by an unclean exit and restores the original proxy.
    ///
    /// Returns `Ok(true)` if a stale marker was found and cleaned up, or `Ok(false)` otherwise.
    pub fn heal_if_crashed(&self) -> Result<bool, SysError> {
        if !self.marker_path.exists() {
            return Ok(false);
        }

        let marker = self.read_marker()?;
        let current = self.backend.get_current()?;
        if current.enable
            && current.host == marker.takeover_host
            && current.port == marker.takeover_port
        {
            self.backend.apply(&marker.original)?;
        }

        remove_file_if_exists(&self.marker_path)?;
        Ok(true)
    }

    fn read_marker(&self) -> Result<TakeoverMarker, SysError> {
        let raw = std::fs::read(&self.marker_path)?;
        Ok(serde_json::from_slice(&raw)?)
    }

    fn write_marker_atomic(&self, marker: &TakeoverMarker) -> Result<(), SysError> {
        let parent = self
            .marker_path
            .parent()
            .ok_or_else(|| std::io::Error::other("takeover marker path has no parent"))?;
        std::fs::create_dir_all(parent)?;

        let json = serde_json::to_vec_pretty(marker)?;
        let mut tmp = NamedTempFile::new_in(parent)?;
        tmp.write_all(&json)?;
        tmp.flush()?;
        tmp.as_file().sync_all()?;
        tmp.persist(&self.marker_path).map_err(|err| err.error)?;
        Ok(())
    }
}

fn join_bypass_list(bypass: &[String]) -> String {
    #[cfg(windows)]
    let sep = ";";
    #[cfg(not(windows))]
    let sep = ",";

    bypass
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>()
        .join(sep)
}

fn remove_file_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn enable_writes_marker_before_apply_and_disable_restores_original() {
        let tmp = TempDir::new().unwrap();
        let initial = SystemProxySnapshot {
            enable: false,
            host: "192.168.1.10".into(),
            port: 8888,
            bypass: "localhost".into(),
        };
        let backend = InMemorySystemProxyBackend::new(initial.clone());
        let manager = SystemProxyManager::with_backend(tmp.path(), backend.clone());

        assert!(!manager.marker_path().exists());

        let bypass = vec!["127.0.0.1".into(), "localhost".into()];
        manager
            .enable("127.0.0.1", 7890, &bypass)
            .expect("enable should succeed");

        assert!(manager.marker_path().exists());
        let current = backend.snapshot();
        assert!(current.enable);
        assert_eq!(current.host, "127.0.0.1");
        assert_eq!(current.port, 7890);

        manager.disable().expect("disable should succeed");
        assert!(!manager.marker_path().exists());
        assert_eq!(backend.snapshot(), initial);

        // Second disable is a harmless no-op.
        manager
            .disable()
            .expect("idempotent disable should succeed");
    }

    #[test]
    fn heal_if_crashed_restores_proxy_when_takeover_still_active() {
        let tmp = TempDir::new().unwrap();
        let original = SystemProxySnapshot {
            enable: true,
            host: "corp-proxy.internal".into(),
            port: 3128,
            bypass: "*.internal".into(),
        };
        let backend = InMemorySystemProxyBackend::new(original.clone());
        let manager = SystemProxyManager::with_backend(tmp.path(), backend.clone());

        assert!(!manager.heal_if_crashed().unwrap());

        manager
            .enable("127.0.0.1", 7897, &["localhost".into()])
            .unwrap();
        assert!(manager.marker_path().exists());

        // Simulate restart with a fresh manager observing the crash marker still on disk.
        let restarted = SystemProxyManager::with_backend(tmp.path(), backend.clone());
        assert!(restarted.heal_if_crashed().unwrap());
        assert!(!restarted.marker_path().exists());
        assert_eq!(backend.snapshot(), original);
    }

    #[test]
    fn heal_if_crashed_preserves_user_modified_proxy_and_clears_stale_marker() {
        let tmp = TempDir::new().unwrap();
        let original = SystemProxySnapshot::default();
        let backend = InMemorySystemProxyBackend::new(original);
        let manager = SystemProxyManager::with_backend(tmp.path(), backend.clone());

        manager.enable("127.0.0.1", 7897, &[]).unwrap();

        // User manually switched to a different proxy while crashed.
        let manual = SystemProxySnapshot {
            enable: true,
            host: "10.0.0.2".into(),
            port: 9000,
            bypass: String::new(),
        };
        backend.apply(&manual).unwrap();

        let restarted = SystemProxyManager::with_backend(tmp.path(), backend.clone());
        assert!(restarted.heal_if_crashed().unwrap());
        assert!(!restarted.marker_path().exists());
        assert_eq!(backend.snapshot(), manual);
    }
}
