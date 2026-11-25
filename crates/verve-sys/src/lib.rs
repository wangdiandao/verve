//! OS-level integration primitives for Verve.
//!
//! Provides host event broadcasting ([`OsEvent`], [`OsEventBus`]), system proxy takeover
//! and crash self-healing ([`SystemProxyManager`]), single-instance locking and primary
//! window activation signaling ([`SingleInstanceGuard`], [`ActivationListener`]),
//! login auto-launch ([`AutoLaunchManager`]), and cross-platform tray adapters ([`TrayBackend`]).

pub mod autolaunch;
pub mod events;
pub mod instance;
pub mod sysproxy;
pub mod tray;

use thiserror::Error;

pub use autolaunch::{
    AutoLaunchBackend, AutoLaunchManager, InMemoryAutoLaunchBackend, OsAutoLaunchBackend,
};
pub use events::{DEFAULT_EVENT_BUS_CAPACITY, OsEvent, OsEventBus};
pub use instance::{
    ActivationListener, SingleInstanceGuard, default_activation_endpoint, notify_primary_instance,
};
pub use sysproxy::{
    InMemorySystemProxyBackend, OsSystemProxyBackend, SystemProxyBackend, SystemProxyManager,
    SystemProxySnapshot, TakeoverMarker,
};
#[cfg(target_os = "linux")]
pub use tray::LinuxKsniTray;
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub use tray::NativeTray;
pub use tray::{
    HeadlessTrayBackend, TrayAction, TrayBackend, TrayIconState, TrayMenuState,
    create_platform_tray, render_tray_icon_rgba,
};

/// Errors returned by OS integration adapters.
#[derive(Debug, Error)]
pub enum SysError {
    /// Requested OS integration capability is not supported on this platform.
    #[error("OS integration capability is not supported: {0}")]
    Unsupported(&'static str),
    /// System proxy backend error.
    #[error("system proxy error: {0}")]
    Proxy(String),
    /// Single-instance guard error.
    #[error("single-instance error: {0}")]
    SingleInstance(String),
    /// Auto-launch adapter error.
    #[error("auto-launch error: {0}")]
    AutoLaunch(String),
    /// System tray backend error.
    #[error("tray adapter error: {0}")]
    Tray(String),
    /// JSON serialization or deserialization failed.
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    /// Underlying I/O error returned by platform APIs.
    #[error("OS integration I/O error: {0}")]
    Io(#[from] std::io::Error),
}
