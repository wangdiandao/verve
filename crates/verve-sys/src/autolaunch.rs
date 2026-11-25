//! Cross-platform OS auto-launch adapter with an in-memory backend for tests.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use auto_launch::{AutoLaunch, AutoLaunchBuilder};

use crate::SysError;

/// Platform abstraction for querying and toggling login auto-launch.
pub trait AutoLaunchBackend: Send + Sync {
    /// Returns `true` if login auto-launch is currently enabled.
    fn is_enabled(&self) -> Result<bool, SysError>;

    /// Enables or disables login auto-launch.
    fn set_enabled(&self, enabled: bool) -> Result<(), SysError>;
}

/// Production [`AutoLaunchBackend`] backed by the `auto-launch` crate.
#[derive(Debug, Clone)]
pub struct OsAutoLaunchBackend {
    inner: AutoLaunch,
}

impl OsAutoLaunchBackend {
    /// Builds a platform auto-launch handle for `app_name` pointing to `app_path` with `args`.
    pub fn new(app_name: &str, app_path: &Path, args: &[&str]) -> Result<Self, SysError> {
        let app_path_str = app_path.to_string_lossy();
        let mut builder = AutoLaunchBuilder::new();
        builder
            .set_app_name(app_name)
            .set_app_path(app_path_str.as_ref())
            .set_args(args);

        #[cfg(windows)]
        builder.set_windows_enable_mode(auto_launch::WindowsEnableMode::CurrentUser);

        let inner = builder
            .build()
            .map_err(|err| SysError::AutoLaunch(err.to_string()))?;
        Ok(Self { inner })
    }
}

impl AutoLaunchBackend for OsAutoLaunchBackend {
    fn is_enabled(&self) -> Result<bool, SysError> {
        self.inner
            .is_enabled()
            .map_err(|err| SysError::AutoLaunch(err.to_string()))
    }

    fn set_enabled(&self, enabled: bool) -> Result<(), SysError> {
        if enabled {
            self.inner
                .enable()
                .map_err(|err| SysError::AutoLaunch(err.to_string()))
        } else {
            self.inner
                .disable()
                .map_err(|err| SysError::AutoLaunch(err.to_string()))
        }
    }
}

/// In-memory [`AutoLaunchBackend`] for tests that must not touch host registry/autostart files.
#[derive(Debug, Clone, Default)]
pub struct InMemoryAutoLaunchBackend {
    enabled: Arc<AtomicBool>,
}

impl InMemoryAutoLaunchBackend {
    /// Creates an in-memory backend with initial state `initial`.
    pub fn new(initial: bool) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(initial)),
        }
    }
}

impl AutoLaunchBackend for InMemoryAutoLaunchBackend {
    fn is_enabled(&self) -> Result<bool, SysError> {
        Ok(self.enabled.load(Ordering::SeqCst))
    }

    fn set_enabled(&self, enabled: bool) -> Result<(), SysError> {
        self.enabled.store(enabled, Ordering::SeqCst);
        Ok(())
    }
}

/// High-level login auto-launch manager wrapping an [`AutoLaunchBackend`].
#[derive(Debug, Clone)]
pub struct AutoLaunchManager<B: AutoLaunchBackend = OsAutoLaunchBackend> {
    backend: B,
}

impl<B: AutoLaunchBackend> AutoLaunchManager<B> {
    /// Creates a new manager wrapping `backend`.
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    /// Returns `true` if auto-launch is currently enabled.
    pub fn is_enabled(&self) -> Result<bool, SysError> {
        self.backend.is_enabled()
    }

    /// Sets the auto-launch state to `enabled`.
    pub fn set_enabled(&self, enabled: bool) -> Result<(), SysError> {
        self.backend.set_enabled(enabled)
    }

    /// Toggles auto-launch and returns the resulting state.
    pub fn toggle(&self) -> Result<bool, SysError> {
        let next = !self.is_enabled()?;
        self.set_enabled(next)?;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_auto_launch_toggles_cleanly() {
        let backend = InMemoryAutoLaunchBackend::new(false);
        let manager = AutoLaunchManager::new(backend);

        assert!(!manager.is_enabled().unwrap());
        assert!(manager.toggle().unwrap());
        assert!(manager.is_enabled().unwrap());

        manager.set_enabled(false).unwrap();
        assert!(!manager.is_enabled().unwrap());
    }
}
