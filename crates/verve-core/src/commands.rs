//! Typed CQRS [`AppCommand`] messages and [`CommandBus`] client handle.

use std::collections::BTreeMap;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use verve_compiler::CommittedConfig;
use verve_config::{ProfileEntry, TunConfig};
use verve_kernel::{ProxyMode, ProxyTopology, RuleSnapshot};

use crate::error::CoreError;
use crate::snapshot::ServiceStatus;

/// Map of proxy node names to their measured round-trip latency in milliseconds (`None` if failed/timed out).
pub type BatchDelayResults = BTreeMap<String, Option<u16>>;

/// Lifecycle action dispatched against the privileged `verve-service` daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceControlAction {
    /// Probe the `verve-service` IPC endpoint and publish `RuntimeSlice::service_status`.
    CheckStatus,
    /// Install `verve-service` into the host OS service manager and re-probe its status.
    Install,
    /// Uninstall `verve-service` from the host OS service manager and re-probe its status.
    Uninstall,
}

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
    /// Downloads, validates, and registers a remote YAML subscription profile.
    ImportRemoteProfile {
        /// Remote HTTP/HTTPS subscription URL.
        url: String,
        /// Optional custom display name overriding HTTP header/URL inference.
        name: Option<String>,
        /// Optional reply channel receiving the newly registered [`ProfileEntry`].
        reply: Option<oneshot::Sender<Result<ProfileEntry, CoreError>>>,
    },
    /// Re-fetches and validates an existing remote subscription profile, reloading if active and running.
    RefreshProfile {
        /// Target profile identifier.
        profile_id: String,
        /// Optional reply channel receiving the refreshed [`ProfileEntry`].
        reply: Option<oneshot::Sender<Result<ProfileEntry, CoreError>>>,
    },
    /// Deletes a profile from `settings.toml`, removes its files and SQLite metadata, and stops kernel if active.
    DeleteProfile {
        /// Target profile identifier.
        profile_id: String,
        /// Optional reply channel receiving completion status.
        reply: Option<oneshot::Sender<Result<(), CoreError>>>,
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
    /// Stops the kernel if running and starts it again cleanly.
    RestartKernel {
        /// Optional reply channel receiving the restarted kernel PID.
        reply: Option<oneshot::Sender<Result<u32, CoreError>>>,
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
    /// Concurrently tests latency of all leaf nodes inside `group`, caches successes, and publishes `ProxiesSlice`.
    TestGroupDelay {
        /// Target proxy group name.
        group: String,
        /// Target probe URL.
        url: String,
        /// Per-node probe timeout duration.
        timeout: Duration,
        /// Optional reply channel receiving node-to-delay results (`None` for timed out / failed probes).
        reply: Option<oneshot::Sender<Result<BatchDelayResults, CoreError>>>,
    },
    /// Concurrently tests latency of all unique leaf nodes across all visible groups.
    TestAllGroupsDelay {
        /// Target probe URL.
        url: String,
        /// Per-node probe timeout duration.
        timeout: Duration,
        /// Optional reply channel receiving node-to-delay results (`None` for timed out / failed probes).
        reply: Option<oneshot::Sender<Result<BatchDelayResults, CoreError>>>,
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
    /// Updates inbound network settings in `settings.toml`, re-applies system proxy, and reloads running kernel.
    UpdateNetworkSettings {
        /// Combined HTTP/SOCKS5 listening port (`mixed-port`, must be `> 0`).
        mixed_port: u16,
        /// Whether LAN clients are permitted (`allow-lan`).
        allow_lan: bool,
        /// Whether IPv6 resolution and forwarding are enabled.
        ipv6: bool,
        /// Host/CIDR patterns bypassed by the OS system proxy.
        system_proxy_bypass: Vec<String>,
        /// Optional reply channel receiving completion status.
        reply: Option<oneshot::Sender<Result<(), CoreError>>>,
    },
    /// Updates TUN parameters in `settings.toml` and `tun_interface_name` in `local.toml`.
    UpdateTunSettings {
        /// Validated syncable TUN configuration.
        tun: TunConfig,
        /// Optional machine-local TUN interface device name (`local.toml`).
        tun_interface_name: Option<String>,
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
    /// Persists optional Material 3 dynamic-color seed ARGB (`0xFFRRGGBB`) in `settings.toml`.
    SetThemeSeedColor {
        /// Optional seed ARGB color (`None` reverts to default).
        seed_argb: Option<u32>,
        /// Optional reply channel receiving completion status.
        reply: Option<oneshot::Sender<Result<(), CoreError>>>,
    },
    /// Validates and persists UI `locale` (`"zh-CN"` or `"en-US"`) in `settings.toml`.
    SetLocale {
        /// Target locale tag string.
        locale: String,
        /// Optional reply channel receiving completion status.
        reply: Option<oneshot::Sender<Result<(), CoreError>>>,
    },
    /// Toggles OS login auto-launch and persists `local.auto_launch`.
    SetAutoLaunch {
        /// Target auto-launch state.
        enabled: bool,
        /// Optional reply channel receiving completion status.
        reply: Option<oneshot::Sender<Result<(), CoreError>>>,
    },
    /// Enables or disables privileged service mode in `local.toml` and syncs kernel supervision.
    SetServiceMode {
        /// Target service mode state.
        enabled: bool,
        /// Optional reply channel receiving completion status.
        reply: Option<oneshot::Sender<Result<(), CoreError>>>,
    },
    /// Probes, installs, or uninstalls the privileged `verve-service` daemon.
    ServiceAction {
        /// Requested service control action.
        action: ServiceControlAction,
        /// Optional reply channel receiving the resulting [`ServiceStatus`].
        reply: Option<oneshot::Sender<Result<ServiceStatus, CoreError>>>,
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

    /// Downloads, validates, and registers a remote YAML subscription profile.
    pub async fn import_remote_profile(
        &self,
        url: impl Into<String>,
        name: Option<String>,
    ) -> Result<ProfileEntry, CoreError> {
        self.call(|reply| AppCommand::ImportRemoteProfile {
            url: url.into(),
            name,
            reply: Some(reply),
        })
        .await
    }

    /// Re-fetches and validates an existing remote subscription profile, preserving disk contents on error.
    pub async fn refresh_profile(
        &self,
        profile_id: impl Into<String>,
    ) -> Result<ProfileEntry, CoreError> {
        self.call(|reply| AppCommand::RefreshProfile {
            profile_id: profile_id.into(),
            reply: Some(reply),
        })
        .await
    }

    /// Deletes a profile, its files, and SQLite metadata, stopping the kernel if deleting the active profile.
    pub async fn delete_profile(&self, profile_id: impl Into<String>) -> Result<(), CoreError> {
        self.call(|reply| AppCommand::DeleteProfile {
            profile_id: profile_id.into(),
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

    /// Restarts the supervised proxy kernel process cleanly.
    pub async fn restart_kernel(&self) -> Result<u32, CoreError> {
        self.call(|reply| AppCommand::RestartKernel { reply: Some(reply) })
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

    /// Concurrently probes latency of all leaf nodes inside `group`.
    pub async fn test_group_delay(
        &self,
        group: impl Into<String>,
        url: impl Into<String>,
        timeout: Duration,
    ) -> Result<BTreeMap<String, Option<u16>>, CoreError> {
        self.call(|reply| AppCommand::TestGroupDelay {
            group: group.into(),
            url: url.into(),
            timeout,
            reply: Some(reply),
        })
        .await
    }

    /// Concurrently probes latency of all unique leaf nodes across all visible proxy groups.
    pub async fn test_all_groups_delay(
        &self,
        url: impl Into<String>,
        timeout: Duration,
    ) -> Result<BTreeMap<String, Option<u16>>, CoreError> {
        self.call(|reply| AppCommand::TestAllGroupsDelay {
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

    /// Validates and persists inbound network settings, re-applying system proxy and reloading kernel if active.
    pub async fn update_network_settings(
        &self,
        mixed_port: u16,
        allow_lan: bool,
        ipv6: bool,
        system_proxy_bypass: Vec<String>,
    ) -> Result<(), CoreError> {
        self.call(|reply| AppCommand::UpdateNetworkSettings {
            mixed_port,
            allow_lan,
            ipv6,
            system_proxy_bypass,
            reply: Some(reply),
        })
        .await
    }

    /// Validates and persists TUN settings (`settings.toml`) and device name (`local.toml`), reloading if active.
    pub async fn update_tun_settings(
        &self,
        tun: TunConfig,
        tun_interface_name: Option<String>,
    ) -> Result<(), CoreError> {
        self.call(|reply| AppCommand::UpdateTunSettings {
            tun,
            tun_interface_name,
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

    /// Persists optional Material 3 dynamic-color seed ARGB (`0xFFRRGGBB`) in `settings.toml`.
    pub async fn set_theme_seed_color(&self, seed_argb: Option<u32>) -> Result<(), CoreError> {
        self.call(|reply| AppCommand::SetThemeSeedColor {
            seed_argb,
            reply: Some(reply),
        })
        .await
    }

    /// Validates and persists `syncable.locale` (`"zh-CN"` or `"en-US"`).
    pub async fn set_locale(&self, locale: impl Into<String>) -> Result<(), CoreError> {
        self.call(|reply| AppCommand::SetLocale {
            locale: locale.into(),
            reply: Some(reply),
        })
        .await
    }

    /// Toggles OS login auto-launch and persists `local.auto_launch`.
    pub async fn set_auto_launch(&self, enabled: bool) -> Result<(), CoreError> {
        self.call(|reply| AppCommand::SetAutoLaunch {
            enabled,
            reply: Some(reply),
        })
        .await
    }

    /// Persists `local.service_mode_enabled` and synchronizes kernel supervision mode.
    pub async fn set_service_mode(&self, enabled: bool) -> Result<(), CoreError> {
        self.call(|reply| AppCommand::SetServiceMode {
            enabled,
            reply: Some(reply),
        })
        .await
    }

    /// Executes a `verve-service` lifecycle action and publishes updated `RuntimeSlice::service_status`.
    pub async fn service_action(
        &self,
        action: ServiceControlAction,
    ) -> Result<ServiceStatus, CoreError> {
        self.call(|reply| AppCommand::ServiceAction {
            action,
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
