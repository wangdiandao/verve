//! Domain errors emitted by [`crate::AppCoordinator`] and [`crate::CommandBus`].

use thiserror::Error;
use verve_compiler::CompileDiagnostic;
use verve_config::ConfigError;
use verve_kernel::KernelError;

/// Errors produced by core orchestration, configuration compilation, and kernel control.
#[derive(Debug, Error)]
pub enum CoreError {
    /// Persistence, validation, or SQLite state error from `verve-config`.
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),

    /// Layered configuration compilation or kernel preflight failed.
    #[error("profile compilation failed: {0}")]
    Compile(#[from] CompileDiagnostic),

    /// Proxy kernel process supervision or IPC control-plane error.
    #[error("kernel error: {0}")]
    Kernel(#[from] KernelError),

    /// Requested profile identifier does not exist in `SyncableConfig`.
    #[error("profile `{0}` not found")]
    ProfileNotFound(String),

    /// Operation requires an active profile, but none is selected.
    #[error("no active profile is selected")]
    NoActiveProfile,

    /// No usable Mihomo executable could be located.
    #[error("mihomo kernel binary could not be located")]
    KernelBinaryNotFound,

    /// Coordinator command channel or reply channel closed before completing the request.
    #[error("coordinator command channel closed")]
    ChannelClosed,
}
