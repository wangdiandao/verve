//! Asynchronous command dispatcher bridging GPUI actions to [`CommandBus`] on Tokio.

use std::future::Future;
use std::time::Duration;

use verve_core::config::TunConfig;
use verve_core::kernel::ProxyMode;
use verve_core::{CommandBus, CoreError, ServiceControlAction};

/// Transient user-facing notification forwarded to shell snackbars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastNotification {
    /// Human-readable notification message.
    pub message: String,
    /// When `true`, rendered with danger/error tone; otherwise neutral/info tone.
    pub is_error: bool,
}

/// Clonable command dispatcher used by GPUI views and the tray controller.
///
/// Spawns async [`CommandBus`] invocations onto the Tokio runtime and forwards any
/// returned [`CoreError`] message (or info notifications) to active shell snackbars.
#[derive(Clone)]
pub struct CommandDispatcher {
    bus: CommandBus,
    tokio: tokio::runtime::Handle,
    error_tx: async_channel::Sender<String>,
    error_rx: async_channel::Receiver<String>,
    toast_tx: async_channel::Sender<ToastNotification>,
    toast_rx: async_channel::Receiver<ToastNotification>,
}

impl CommandDispatcher {
    /// Creates a new [`CommandDispatcher`] bound to `bus` and `tokio`.
    pub fn new(bus: CommandBus, tokio: tokio::runtime::Handle) -> Self {
        let (error_tx, error_rx) = async_channel::bounded(32);
        let (toast_tx, toast_rx) = async_channel::bounded(32);
        Self {
            bus,
            tokio,
            error_tx,
            error_rx,
            toast_tx,
            toast_rx,
        }
    }

    /// Returns a receiver yielding formatted error messages from failed commands.
    pub fn subscribe_errors(&self) -> async_channel::Receiver<String> {
        self.error_rx.clone()
    }

    /// Returns a receiver yielding both error and informational [`ToastNotification`]s.
    pub fn subscribe_toasts(&self) -> async_channel::Receiver<ToastNotification> {
        self.toast_rx.clone()
    }

    /// Emits a non-error informational/success toast notification to active shell snackbars.
    pub fn notify_info(&self, message: impl Into<String>) {
        let _ = self.toast_tx.try_send(ToastNotification {
            message: message.into(),
            is_error: false,
        });
    }

    /// Returns a reference to the underlying [`CommandBus`].
    pub fn bus(&self) -> &CommandBus {
        &self.bus
    }

    /// Returns a reference to the Tokio runtime handle.
    pub fn tokio_handle(&self) -> &tokio::runtime::Handle {
        &self.tokio
    }

    /// Switches the global proxy routing mode (`Rule`, `Global`, `Direct`).
    pub fn set_proxy_mode(&self, mode: ProxyMode) {
        let bus = self.bus.clone();
        self.spawn_unit(
            "set_proxy_mode",
            async move { bus.set_proxy_mode(mode).await },
        );
    }

    /// Enables or disables OS system proxy takeover.
    pub fn set_system_proxy(&self, enabled: bool) {
        let bus = self.bus.clone();
        self.spawn_unit("set_system_proxy", async move {
            bus.set_system_proxy(enabled).await
        });
    }

    /// Enables or disables TUN virtual interface capture.
    pub fn set_tun_enabled(&self, enabled: bool) {
        let bus = self.bus.clone();
        self.spawn_unit("set_tun_enabled", async move {
            bus.set_tun_enabled(enabled).await
        });
    }

    /// Starts the supervised proxy kernel process.
    pub fn start_kernel(&self) {
        let bus = self.bus.clone();
        self.spawn_result("start_kernel", async move { bus.start_kernel().await });
    }

    /// Stops the supervised proxy kernel process.
    pub fn stop_kernel(&self) {
        let bus = self.bus.clone();
        self.spawn_unit("stop_kernel", async move { bus.stop_kernel().await });
    }

    /// Restarts the supervised proxy kernel process cleanly.
    pub fn restart_kernel(&self) {
        let bus = self.bus.clone();
        self.spawn_result("restart_kernel", async move { bus.restart_kernel().await });
    }

    /// Refreshes proxy group and node topology from the running kernel.
    pub fn refresh_proxies(&self) {
        let bus = self.bus.clone();
        self.spawn_result(
            "refresh_proxies",
            async move { bus.refresh_proxies().await },
        );
    }

    /// Selects `node` within selector `group`.
    pub fn select_proxy(&self, group: impl Into<String>, node: impl Into<String>) {
        let bus = self.bus.clone();
        let group = group.into();
        let node = node.into();
        self.spawn_unit("select_proxy", async move {
            bus.select_proxy(group, node).await
        });
    }

    /// Probes round-trip latency for `node` against `url` with `timeout`.
    pub fn test_delay(&self, node: impl Into<String>, url: impl Into<String>, timeout: Duration) {
        let bus = self.bus.clone();
        let node = node.into();
        let url = url.into();
        self.spawn_result("test_delay", async move {
            bus.test_delay(node, url, timeout).await
        });
    }

    /// Concurrently probes round-trip latency for all leaf nodes inside `group`.
    pub fn test_group_delay(
        &self,
        group: impl Into<String>,
        url: impl Into<String>,
        timeout: Duration,
    ) {
        let bus = self.bus.clone();
        let group = group.into();
        let url = url.into();
        self.spawn_result("test_group_delay", async move {
            bus.test_group_delay(group, url, timeout).await
        });
    }

    /// Concurrently probes round-trip latency for all unique leaf nodes across visible groups.
    pub fn test_all_groups_delay(&self, url: impl Into<String>, timeout: Duration) {
        let bus = self.bus.clone();
        let url = url.into();
        self.spawn_result("test_all_groups_delay", async move {
            bus.test_all_groups_delay(url, timeout).await
        });
    }

    /// Imports or updates a local profile YAML (and optional merge overlay).
    pub fn import_local_profile(
        &self,
        id: impl Into<String>,
        name: impl Into<String>,
        raw_yaml: impl Into<String>,
        merge_yaml: Option<String>,
    ) {
        let bus = self.bus.clone();
        let id = id.into();
        let name = name.into();
        let raw_yaml = raw_yaml.into();
        self.spawn_result("import_local_profile", async move {
            bus.import_local_profile(id, name, raw_yaml, merge_yaml)
                .await
        });
    }

    /// Downloads, validates, and registers a remote YAML subscription profile.
    pub fn import_remote_profile(&self, url: impl Into<String>, name: Option<String>) {
        let bus = self.bus.clone();
        let url = url.into();
        self.spawn_result("import_remote_profile", async move {
            bus.import_remote_profile(url, name).await
        });
    }

    /// Re-fetches and validates an existing remote subscription profile.
    pub fn refresh_profile(&self, profile_id: impl Into<String>) {
        let bus = self.bus.clone();
        let profile_id = profile_id.into();
        self.spawn_result("refresh_profile", async move {
            bus.refresh_profile(profile_id).await
        });
    }

    /// Deletes a profile, its files, and SQLite metadata.
    pub fn delete_profile(&self, profile_id: impl Into<String>) {
        let bus = self.bus.clone();
        let profile_id = profile_id.into();
        self.spawn_unit("delete_profile", async move {
            bus.delete_profile(profile_id).await
        });
    }

    /// Compiles, preflight-validates, and activates `profile_id`.
    pub fn activate_profile(&self, profile_id: impl Into<String>) {
        let bus = self.bus.clone();
        let profile_id = profile_id.into();
        self.spawn_result("activate_profile", async move {
            bus.activate_profile(profile_id).await
        });
    }

    /// Validates and persists inbound network settings, re-applying system proxy and reloading if running.
    pub fn update_network_settings(
        &self,
        mixed_port: u16,
        allow_lan: bool,
        ipv6: bool,
        system_proxy_bypass: Vec<String>,
    ) {
        let bus = self.bus.clone();
        self.spawn_unit("update_network_settings", async move {
            bus.update_network_settings(mixed_port, allow_lan, ipv6, system_proxy_bypass)
                .await
        });
    }

    /// Validates and persists TUN parameters and device interface name.
    pub fn update_tun_settings(&self, tun: TunConfig, tun_interface_name: Option<String>) {
        let bus = self.bus.clone();
        self.spawn_unit("update_tun_settings", async move {
            bus.update_tun_settings(tun, tun_interface_name).await
        });
    }

    /// Persists `syncable.theme_mode` (`"light"`, `"dark"`, or `"system"`).
    pub fn set_theme_mode(&self, mode: impl Into<String>) {
        let bus = self.bus.clone();
        let mode = mode.into();
        self.spawn_unit(
            "set_theme_mode",
            async move { bus.set_theme_mode(mode).await },
        );
    }

    /// Persists optional Material 3 dynamic-color seed ARGB (`0xFFRRGGBB`) in `settings.toml`.
    pub fn set_theme_seed_color(&self, seed_argb: Option<u32>) {
        let bus = self.bus.clone();
        self.spawn_unit("set_theme_seed_color", async move {
            bus.set_theme_seed_color(seed_argb).await
        });
    }

    /// Persists `syncable.locale` (`"zh-CN"` or `"en-US"`).
    pub fn set_locale(&self, locale: impl Into<String>) {
        let bus = self.bus.clone();
        let locale = locale.into();
        self.spawn_unit("set_locale", async move { bus.set_locale(locale).await });
    }

    /// Toggles OS login auto-launch and persists `local.auto_launch`.
    pub fn set_auto_launch(&self, enabled: bool) {
        let bus = self.bus.clone();
        self.spawn_unit("set_auto_launch", async move {
            bus.set_auto_launch(enabled).await
        });
    }

    /// Persists `local.service_mode_enabled` and synchronizes kernel supervision.
    pub fn set_service_mode(&self, enabled: bool) {
        let bus = self.bus.clone();
        self.spawn_unit("set_service_mode", async move {
            bus.set_service_mode(enabled).await
        });
    }

    /// Dispatches a `verve-service` lifecycle action (`CheckStatus`, `Install`, `Uninstall`).
    pub fn service_action(&self, action: ServiceControlAction) {
        let bus = self.bus.clone();
        self.spawn_result(
            "service_action",
            async move { bus.service_action(action).await },
        );
    }

    /// Refreshes active routing rules and rule providers into `ActivitySlice`.
    pub fn refresh_rules(&self) {
        let bus = self.bus.clone();
        self.spawn_result("refresh_rules", async move { bus.refresh_rules().await });
    }

    /// Gracefully shuts down the backend coordinator.
    pub async fn shutdown(&self) -> Result<(), CoreError> {
        self.bus.shutdown().await
    }

    fn spawn_unit<F>(&self, label: &'static str, fut: F)
    where
        F: Future<Output = Result<(), CoreError>> + Send + 'static,
    {
        self.spawn_result(label, fut);
    }

    fn spawn_result<T, F>(&self, label: &'static str, fut: F)
    where
        F: Future<Output = Result<T, CoreError>> + Send + 'static,
    {
        let err_tx = self.error_tx.clone();
        let toast_tx = self.toast_tx.clone();
        self.tokio.spawn(async move {
            if let Err(err) = fut.await {
                tracing::warn!(command = label, error = %err, "coordinator command failed");
                let message = err.to_string();
                let _ = err_tx.try_send(message.clone());
                let _ = toast_tx.try_send(ToastNotification {
                    message,
                    is_error: true,
                });
            }
        });
    }
}
