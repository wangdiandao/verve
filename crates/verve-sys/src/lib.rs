//! OS-level integration primitives for Verve.
//!
//! Full platform adapters (system proxy takeover and crash self-healing,
//! auto-launch, tray, power/network listeners, and single-instance guards)
//! are implemented in `0.0.2`.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Normalized host power and network topology events emitted by platform listeners.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OsEvent {
    /// The host resumed from sleep or hibernation.
    WakeUp,
    /// Active network interfaces or routing tables changed.
    NetworkChanged,
}

/// Errors returned by OS integration adapters.
#[derive(Debug, Error)]
pub enum SysError {
    /// Requested OS integration capability is not yet implemented on this platform.
    #[error("OS integration capability is not yet implemented: {0}")]
    Unsupported(&'static str),
    /// Underlying I/O error returned by platform APIs.
    #[error("OS integration I/O error: {0}")]
    Io(#[from] std::io::Error),
}
