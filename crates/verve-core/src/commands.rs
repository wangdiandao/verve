//! Typed CQRS [`AppCommand`] messages and [`CommandBus`] client handle.

use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use verve_compiler::CommittedConfig;
use verve_config::ProfileEntry;
use verve_kernel::{ProxyMode, ProxyTopology, RuleSnapshot};

use crate::error::CoreError;

/// Serialized write/action commands processed by [`crate::AppCoordinator`].
#[derive(Debug)]
pub enum AppCommand {
    /// Imports or updates a local profile YAML (and optional merge overlay) on disk and in `settings.toml`.
    ImportLocalProfile {
        /// Stable unique profile identifier.
        id: String,
        /// Human-readable profile display name.
        name: String,
        /// Raw YAML profile contents.
        raw_yaml: String,
        /// Optional declarative Merge YAML overlay.
        merge_yaml: Option<String>,
        /// Optional reply channel receiving the registered [`ProfileEntry`].
        reply: Option<oneshot::Sender<Result<ProfileEntry, CoreError>>>,
    },
    /// Compiles, preflight-validates, and commits the target profile as active.
    ActivateProfile {
        /// Target profile identifier.
        profile_id: String,
        /// Optional reply channel receiving [`CommittedConfig`].
        reply: Option<oneshot::Sender<Result<CommittedConfig, CoreError>>>,
    },
    /// Compiles (if needed) and starts the supervised proxy kernel process.
    StartKernel {
        /// Optional reply channel receiving the started kernel PID.
        reply: Option<oneshot::Sender<Result<u32, CoreError>>>,
    },
    /// Stops the supervised proxy kernel process.
    StopKernel {
        /// Optional reply channel receiving completion status.
        reply: Option<oneshot::Sender<Result<(), CoreError>>>,
    },
    /// Fetches live proxy topology from the kernel and updates `ProxiesSlice`.
    RefreshProxies {
        /// Optional reply channel receiving [`ProxyTopology`].
        reply: Option<oneshot::Sender<Result<ProxyTopology, CoreError>>>,
    },
    /// Switches the active member inside a proxy selector group and persists the choice.
    SelectProxy {
        /// Selector group name.
        group: String,
        /// Outbound proxy or sub-group name.
        node: String,
        /// Optional reply channel receiving completion status.
        reply: Option<oneshot::Sender<Result<(), CoreError>>>,
    },
    /// Tests round-trip latency of a proxy node, caches it in SQLite, and updates `ProxiesSlice`.
    TestDelay {
        /// Target proxy node name.
        node: String,
        /// Target probe URL.
        url: String,
        /// Probe timeout duration.
        timeout: Duration,
        /// Optional reply channel receiving measured delay in milliseconds.
        reply: Option<oneshot::Sender<Result<u16, CoreError>>>,
    },
    /// Updates global routing mode in `settings.toml` and reloads the running kernel if active.
    SetProxyMode {
        /// Target routing mode.
        mode: ProxyMode,
        /// Optional reply channel receiving completion status.
        reply: Option<oneshot::Sender<Result<(), CoreError>>>,
    },
    /// Enables or disables OS system proxy takeover and persists `settings.toml`.
    SetSystemProxy {
        /// Target system proxy state.
        enabled: bool,
        /// Optional reply channel receiving completion status.
        reply: Option<oneshot::Sender<Result<(), CoreError>>>,
    },
    /// Enables or disables TUN mode, recompiles `active.yaml`, and reloads/restarts kernel supervision.
    SetTunEnabled {
        /// Target TUN mode state.
        enabled: bool,
        /// Optional reply channel receiving completion status.
        reply: Option<oneshot::Sender<Result<(), CoreError>>>,
    },
    /// Validates and persists UI `theme_mode` (`"light"`, `"dark"`, or `"system"`) in `settings.toml`.
    SetThemeMode {
        /// Target theme mode string.
        mode: String,
        /// Optional reply channel receiving completion status.
        reply: Option<oneshot::Sender<Result<(), CoreError>>>,
    },
    /// Fetches active routing rules and external rule providers into `ActivitySlice`.
    RefreshRules {
        /// Optional reply channel receiving [`RuleSnapshot`].
        reply: Option<oneshot::Sender<Result<RuleSnapshot, CoreError>>>,
    },
    /// Cancels telemetry streams, stops the kernel if running, and exits the coordinator loop.
    Shutdown {
        /// Optional reply channel receiving completion status.
        reply: Option<oneshot::Sender<Result<(), CoreError>>>,
    },
}

/// Clonable client handle for dispatching commands into [`crate::AppCoordinator`].
#[derive(Debug, Clone)]
pub struct CommandBus {
    tx: mpsc::Sender<AppCommand>,
}

impl CommandBus {
    pub(crate) fn new(tx: mpsc::Sender<AppCommand>) -> Self {
        Self { tx }
    }

    /// Dispatches a raw [`AppCommand`] asynchronously, waiting for queue capacity if full.
    pub async fn dispatch(&self, command: AppCommand) -> Result<(), CoreError> {
        self.tx
            .send(command)
            .await
            .map_err(|_| CoreError::ChannelClosed)
    }

    /// Attempts to enqueue a raw [`AppCommand`] without awaiting (fire-and-forget).
    pub fn try_dispatch(&self, command: AppCommand) -> Result<(), CoreError> {
        self.tx
            .try_send(command)
            .map_err(|_| CoreError::ChannelClosed)
    }

    /// Imports or updates a local profile YAML (and optional merge overlay).
    pub async fn import_local_profile(
        &self,
        id: impl Into<String>,
        name: impl Into<String>,
        raw_yaml: impl Into<String>,
        merge_yaml: Option<String>,
    ) -> Result<ProfileEntry, CoreError> {
        self.call(|reply| AppCommand::ImportLocalProfile {
            id: id.into(),
            name: name.into(),
            raw_yaml: raw_yaml.into(),
            merge_yaml,
            reply: Some(reply),
        })
        .await
    }

    /// Compiles, preflight-validates, and activates `profile_id`.
    pub async fn activate_profile(
        &self,
        profile_id: impl Into<String>,
    ) -> Result<CommittedConfig, CoreError> {
        self.call(|reply| AppCommand::ActivateProfile {
            profile_id: profile_id.into(),
            reply: Some(reply),
        })
        .await
    }

    /// Starts the supervised proxy kernel process and waits until its control plane responds.
    pub async fn start_kernel(&self) -> Result<u32, CoreError> {
        self.call(|reply| AppCommand::StartKernel { reply: Some(reply) })
            .await
    }

    /// Stops the supervised proxy kernel process.
    pub async fn stop_kernel(&self) -> Result<(), CoreError> {
        self.call(|reply| AppCommand::StopKernel { reply: Some(reply) })
            .await
    }

    /// Refreshes proxy group and node topology from the kernel.
    pub async fn refresh_proxies(&self) -> Result<ProxyTopology, CoreError> {
        self.call(|reply| AppCommand::RefreshProxies { reply: Some(reply) })
            .await
    }

    /// Selects `node` in selector `group` and persists the selection for the active profile.
    pub async fn select_proxy(
        &self,
        group: impl Into<String>,
        node: impl Into<String>,
    ) -> Result<(), CoreError> {
        self.call(|reply| AppCommand::SelectProxy {
            group: group.into(),
            node: node.into(),
            reply: Some(reply),
        })
        .await
    }

    /// Measures latency for `node` against `url`, caches it in SQLite, and updates `ProxiesSlice`.
    pub async fn test_delay(
        &self,
        node: impl Into<String>,
        url: impl Into<String>,
        timeout: Duration,
    ) -> Result<u16, CoreError> {
        self.call(|reply| AppCommand::TestDelay {
            node: node.into(),
            url: url.into(),
            timeout,
            reply: Some(reply),
        })
        .await
    }

    /// Switches global routing mode, persists `settings.toml`, and reloads a running kernel.
    pub async fn set_proxy_mode(&self, mode: ProxyMode) -> Result<(), CoreError> {
        self.call(|reply| AppCommand::SetProxyMode {
            mode,
            reply: Some(reply),
        })
        .await
    }

    /// Enables or disables OS system proxy takeover and persists `settings.toml`.
    pub async fn set_system_proxy(&self, enabled: bool) -> Result<(), CoreError> {
        self.call(|reply| AppCommand::SetSystemProxy {
            enabled,
            reply: Some(reply),
        })
        .await
    }

    /// Enables or disables TUN mode, recompiles `active.yaml`, and reloads/restarts supervision.
    pub async fn set_tun_enabled(&self, enabled: bool) -> Result<(), CoreError> {
        self.call(|reply| AppCommand::SetTunEnabled {
            enabled,
            reply: Some(reply),
        })
        .await
    }

    /// Validates and persists `syncable.theme_mode` (`"light"`, `"dark"`, or `"system"`).
    pub async fn set_theme_mode(&self, mode: impl Into<String>) -> Result<(), CoreError> {
        self.call(|reply| AppCommand::SetThemeMode {
            mode: mode.into(),
            reply: Some(reply),
        })
        .await
    }

    /// Fetches active routing rules and rule providers into `ActivitySlice`.
    pub async fn refresh_rules(&self) -> Result<RuleSnapshot, CoreError> {
        self.call(|reply| AppCommand::RefreshRules { reply: Some(reply) })
            .await
    }

    /// Gracefully shuts down telemetry streams, any running kernel, and the coordinator loop.
    pub async fn shutdown(&self) -> Result<(), CoreError> {
        self.call(|reply| AppCommand::Shutdown { reply: Some(reply) })
            .await
    }

    async fn call<R>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<R, CoreError>>) -> AppCommand,
    ) -> Result<R, CoreError> {
        let (tx, rx) = oneshot::channel();
        let command = build(tx);
        self.tx
            .send(command)
            .await
            .map_err(|_| CoreError::ChannelClosed)?;
        rx.await.map_err(|_| CoreError::ChannelClosed)?
    }
}
