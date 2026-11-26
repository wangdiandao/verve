//! Asynchronous command dispatcher bridging GPUI actions to [`CommandBus`] on Tokio.

use std::future::Future;
use std::time::Duration;

use verve_core::kernel::ProxyMode;
use verve_core::{CommandBus, CoreError};

/// Clonable command dispatcher used by GPUI views and the tray controller.
///
/// Spawns async [`CommandBus`] invocations onto the Tokio runtime and forwards any
/// returned [`CoreError`] message to active shell snackbars.
#[derive(Clone)]
pub struct CommandDispatcher {
    bus: CommandBus,
    tokio: tokio::runtime::Handle,
    error_tx: async_channel::Sender<String>,
    error_rx: async_channel::Receiver<String>,
}

impl CommandDispatcher {
    /// Creates a new [`CommandDispatcher`] bound to `bus` and `tokio`.
    pub fn new(bus: CommandBus, tokio: tokio::runtime::Handle) -> Self {
        let (error_tx, error_rx) = async_channel::bounded(32);
        Self {
            bus,
            tokio,
            error_tx,
            error_rx,
        }
    }

    /// Returns a receiver yielding formatted error messages from failed commands.
    pub fn subscribe_errors(&self) -> async_channel::Receiver<String> {
        self.error_rx.clone()
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

    /// Compiles, preflight-validates, and activates `profile_id`.
    pub fn activate_profile(&self, profile_id: impl Into<String>) {
        let bus = self.bus.clone();
        let profile_id = profile_id.into();
        self.spawn_result("activate_profile", async move {
            bus.activate_profile(profile_id).await
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

    /// Persists `syncable.locale` (`"zh-CN"` or `"en-US"`).
    pub fn set_locale(&self, locale: impl Into<String>) {
        let bus = self.bus.clone();
        let locale = locale.into();
        self.spawn_unit("set_locale", async move { bus.set_locale(locale).await });
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
        self.tokio.spawn(async move {
            if let Err(err) = fut.await {
                tracing::warn!(command = label, error = %err, "coordinator command failed");
                let _ = err_tx.try_send(err.to_string());
            }
        });
    }
}
