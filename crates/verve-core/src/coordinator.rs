//! Single-writer CQRS [`AppCoordinator`] and [`MihomoCoordinatorBuilder`].

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::stream::{self, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use verve_compiler::{
    CommittedConfig, CompileContext, CompilerPipeline, NoopScriptEngine, RuntimeInjectionConfig,
    ScriptEngine, TunInjectionConfig,
};
use verve_config::{
    ConfigError, ConfigManager, LocalMachineConfig, ProfileEntry, ProfileSource, ProfileUsage,
    StateStore, SyncableConfig, TunConfig,
};
use verve_kernel::{
    ControlEndpoint, DaemonIpcSupervisor, DirectProcessSupervisor, KernelError, KernelSupervisor,
    LogLevel, ProxyKernel, ProxyMode, ProxyTopology, RuleSnapshot, ServiceIpcClient,
};
use verve_kernel_mihomo::{MihomoKernel, MihomoKernelConfig, discover_mihomo_binary};
use verve_sys::{
    AutoLaunchBackend, InMemoryAutoLaunchBackend, InMemorySystemProxyBackend, OsAutoLaunchBackend,
    OsSystemProxyBackend, SysError, SystemProxyBackend, SystemProxyManager, SystemProxySnapshot,
};

use crate::commands::{AppCommand, CommandBus, ServiceControlAction};
use crate::error::CoreError;
use crate::service_ipc::{OsServiceControlBackend, ServiceControlBackend};
use crate::snapshot::{
    AppSnapshot, ProfilesSlice, RuntimeSlice, ServiceStatus, SettingsSlice, SnapshotStore,
};
use crate::subscription::{
    ReqwestSubscriptionFetcher, SubscriptionFetcher, generate_profile_id, infer_profile_name,
    validate_profile_yaml,
};
use crate::telemetry::{DEFAULT_LOG_CAPACITY, DEFAULT_TRAFFIC_HISTORY_CAPACITY, TelemetryHub};

const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_COMMAND_BUFFER_CAPACITY: usize = 64;
const MAX_CONCURRENT_DELAY_TESTS: usize = 8;

/// Configuration parameters for spawning an [`AppCoordinator`].
#[derive(Clone)]
pub struct CoordinatorConfig {
    /// Local control-plane IPC endpoint injected into compiled profiles.
    pub control_endpoint: ControlEndpoint,
    /// Optional explicit override path to the proxy kernel binary.
    pub kernel_binary_path: Option<PathBuf>,
    /// Maximum time to wait for a newly spawned kernel's control plane to respond.
    pub startup_timeout: Duration,
    /// Bounded capacity of the coordinator command channel.
    pub command_buffer_capacity: usize,
    /// Maximum number of samples retained in [`crate::TelemetrySlice::traffic_history`].
    pub traffic_history_capacity: usize,
    /// Maximum number of log entries retained in [`crate::ActivitySlice::logs`].
    pub log_capacity: usize,
    /// Optional override backend for OS system proxy takeover (defaults to [`OsSystemProxyBackend`]
    /// or [`InMemorySystemProxyBackend`] when `VERVE_DRY_RUN_SYSPROXY=1`).
    pub system_proxy_backend: Option<Arc<dyn SystemProxyBackend>>,
    /// Optional privileged `verve-service` IPC client used when `local.service_mode_enabled` is `true`.
    pub service_ipc_client: Option<Arc<dyn ServiceIpcClient>>,
    /// Optional override remote subscription HTTP fetcher (defaults to [`ReqwestSubscriptionFetcher`]).
    pub subscription_fetcher: Option<Arc<dyn SubscriptionFetcher>>,
    /// Optional override OS login auto-launch backend (defaults to [`OsAutoLaunchBackend`]
    /// or [`InMemoryAutoLaunchBackend`] when `VERVE_DRY_RUN_SYSPROXY=1`).
    pub auto_launch_backend: Option<Arc<dyn AutoLaunchBackend>>,
    /// Optional override service install/uninstall backend (defaults to [`OsServiceControlBackend`]).
    pub service_control_backend: Option<Arc<dyn ServiceControlBackend>>,
}

impl fmt::Debug for CoordinatorConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoordinatorConfig")
            .field("control_endpoint", &self.control_endpoint)
            .field("kernel_binary_path", &self.kernel_binary_path)
            .field("startup_timeout", &self.startup_timeout)
            .field("command_buffer_capacity", &self.command_buffer_capacity)
            .field("traffic_history_capacity", &self.traffic_history_capacity)
            .field("log_capacity", &self.log_capacity)
            .field(
                "has_custom_system_proxy_backend",
                &self.system_proxy_backend.is_some(),
            )
            .field("has_service_ipc_client", &self.service_ipc_client.is_some())
            .field(
                "has_custom_subscription_fetcher",
                &self.subscription_fetcher.is_some(),
            )
            .field(
                "has_custom_auto_launch_backend",
                &self.auto_launch_backend.is_some(),
            )
            .field(
                "has_custom_service_control_backend",
                &self.service_control_backend.is_some(),
            )
            .finish()
    }
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            control_endpoint: ControlEndpoint::default_for_session("default"),
            kernel_binary_path: None,
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            command_buffer_capacity: DEFAULT_COMMAND_BUFFER_CAPACITY,
            traffic_history_capacity: DEFAULT_TRAFFIC_HISTORY_CAPACITY,
            log_capacity: DEFAULT_LOG_CAPACITY,
            system_proxy_backend: None,
            service_ipc_client: None,
            subscription_fetcher: None,
            auto_launch_backend: None,
            service_control_backend: None,
        }
    }
}

/// Handle to a running [`AppCoordinator`] task exposing its command bus, snapshots, and telemetry hub.
pub struct CoordinatorHandle<K: ProxyKernel> {
    bus: CommandBus,
    snapshots: Arc<SnapshotStore>,
    telemetry: TelemetryHub<K>,
    kernel: Arc<K>,
    task: Option<JoinHandle<()>>,
}

impl<K: ProxyKernel> CoordinatorHandle<K> {
    /// Returns a reference to the clonable [`CommandBus`].
    pub fn bus(&self) -> &CommandBus {
        &self.bus
    }

    /// Returns the shared [`SnapshotStore`].
    pub fn snapshots(&self) -> &Arc<SnapshotStore> {
        &self.snapshots
    }

    /// Returns the current [`AppSnapshot`] via lock-free load.
    pub fn snapshot(&self) -> Arc<AppSnapshot> {
        self.snapshots.snapshot()
    }

    /// Returns the [`TelemetryHub`] for acquiring RAII [`crate::TelemetryLease`] instances.
    pub fn telemetry(&self) -> &TelemetryHub<K> {
        &self.telemetry
    }

    /// Returns a shared handle to the underlying [`ProxyKernel`].
    pub fn kernel(&self) -> &Arc<K> {
        &self.kernel
    }

    /// Shuts down the coordinator cleanly and awaits background task exit.
    pub async fn shutdown(&mut self) -> Result<(), CoreError> {
        let send_res = self.bus.shutdown().await;
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
        send_res
    }
}

/// Ergonomic builder wiring the real Mihomo backend stack into [`AppCoordinator`].
pub struct MihomoCoordinatorBuilder {
    root_dir: PathBuf,
    binary_path: Option<PathBuf>,
    control_endpoint: Option<ControlEndpoint>,
    secret: Option<String>,
    request_timeout: Duration,
    startup_timeout: Duration,
    script_engine: Arc<dyn ScriptEngine>,
    command_buffer_capacity: usize,
    traffic_history_capacity: usize,
    log_capacity: usize,
    system_proxy_backend: Option<Arc<dyn SystemProxyBackend>>,
    service_ipc_client: Option<Arc<dyn ServiceIpcClient>>,
    subscription_fetcher: Option<Arc<dyn SubscriptionFetcher>>,
    auto_launch_backend: Option<Arc<dyn AutoLaunchBackend>>,
    service_control_backend: Option<Arc<dyn ServiceControlBackend>>,
}

impl MihomoCoordinatorBuilder {
    /// Creates a new builder rooted at application data directory `root_dir`.
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
            binary_path: None,
            control_endpoint: None,
            secret: None,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            script_engine: Arc::new(NoopScriptEngine),
            command_buffer_capacity: DEFAULT_COMMAND_BUFFER_CAPACITY,
            traffic_history_capacity: DEFAULT_TRAFFIC_HISTORY_CAPACITY,
            log_capacity: DEFAULT_LOG_CAPACITY,
            system_proxy_backend: None,
            service_ipc_client: None,
            subscription_fetcher: None,
            auto_launch_backend: None,
            service_control_backend: None,
        }
    }

    /// Overrides the Mihomo executable path instead of auto-discovering.
    pub fn binary_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.binary_path = Some(path.into());
        self
    }

    /// Sets the local Named Pipe / Unix Domain Socket control endpoint.
    pub fn control_endpoint(mut self, endpoint: ControlEndpoint) -> Self {
        self.control_endpoint = Some(endpoint);
        self
    }

    /// Sets an optional Bearer secret for Mihomo's controller API.
    pub fn secret(mut self, secret: impl Into<String>) -> Self {
        self.secret = Some(secret.into());
        self
    }

    /// Sets the per-request IPC timeout used by [`MihomoKernel`].
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Sets the startup readiness timeout when launching Mihomo.
    pub fn startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }

    /// Supplies a custom [`ScriptEngine`] for JS profile/global override stages.
    pub fn script_engine(mut self, engine: Arc<dyn ScriptEngine>) -> Self {
        self.script_engine = engine;
        self
    }

    /// Sets the command channel capacity.
    pub fn command_buffer_capacity(mut self, capacity: usize) -> Self {
        self.command_buffer_capacity = capacity.max(1);
        self
    }

    /// Sets the bounded capacity of [`crate::TelemetrySlice::traffic_history`].
    pub fn traffic_history_capacity(mut self, capacity: usize) -> Self {
        self.traffic_history_capacity = capacity;
        self
    }

    /// Sets the bounded capacity of [`crate::ActivitySlice::logs`].
    pub fn log_capacity(mut self, capacity: usize) -> Self {
        self.log_capacity = capacity;
        self
    }

    /// Overrides the OS system proxy backend (useful for dry-run / headless verification).
    pub fn system_proxy_backend(mut self, backend: Arc<dyn SystemProxyBackend>) -> Self {
        self.system_proxy_backend = Some(backend);
        self
    }

    /// Supplies a [`ServiceIpcClient`] used when `local.service_mode_enabled` is enabled.
    pub fn service_ipc_client(mut self, client: Arc<dyn ServiceIpcClient>) -> Self {
        self.service_ipc_client = Some(client);
        self
    }

    /// Overrides the remote subscription HTTP fetcher.
    pub fn subscription_fetcher(mut self, fetcher: Arc<dyn SubscriptionFetcher>) -> Self {
        self.subscription_fetcher = Some(fetcher);
        self
    }

    /// Overrides the OS login auto-launch backend.
    pub fn auto_launch_backend(mut self, backend: Arc<dyn AutoLaunchBackend>) -> Self {
        self.auto_launch_backend = Some(backend);
        self
    }

    /// Overrides the OS service install/uninstall backend.
    pub fn service_control_backend(mut self, backend: Arc<dyn ServiceControlBackend>) -> Self {
        self.service_control_backend = Some(backend);
        self
    }

    /// Opens configuration storage, resolves the Mihomo binary, and spawns [`AppCoordinator`].
    pub fn spawn(self) -> Result<CoordinatorHandle<MihomoKernel>, CoreError> {
        let config_manager = ConfigManager::open(&self.root_dir)?;
        let local = config_manager.load_local()?;
        let binary_path = self
            .binary_path
            .or(local.mihomo_binary_path)
            .or_else(|| discover_mihomo_binary(None))
            .ok_or(CoreError::KernelBinaryNotFound)?;

        let control_endpoint = self
            .control_endpoint
            .unwrap_or_else(|| ControlEndpoint::default_for_session("default"));

        let kernel = Arc::new(MihomoKernel::new(MihomoKernelConfig {
            binary_path: binary_path.clone(),
            work_dir: config_manager.runtime_dir(),
            endpoint: control_endpoint.clone(),
            secret: self.secret,
            request_timeout: self.request_timeout,
        }));

        let compiler = CompilerPipeline::new(self.script_engine);
        let supervisor = if local.service_mode_enabled
            && let Some(ref client) = self.service_ipc_client
        {
            KernelSupervisor::Service(DaemonIpcSupervisor::new(
                Arc::clone(client),
                "mihomo",
                control_endpoint.as_str_lossy().into_owned(),
            ))
        } else {
            KernelSupervisor::Direct(DirectProcessSupervisor::new())
        };
        let coord_config = CoordinatorConfig {
            control_endpoint,
            kernel_binary_path: Some(binary_path),
            startup_timeout: self.startup_timeout,
            command_buffer_capacity: self.command_buffer_capacity,
            traffic_history_capacity: self.traffic_history_capacity,
            log_capacity: self.log_capacity,
            system_proxy_backend: self.system_proxy_backend,
            service_ipc_client: self.service_ipc_client,
            subscription_fetcher: self.subscription_fetcher,
            auto_launch_backend: self.auto_launch_backend,
            service_control_backend: self.service_control_backend,
        };

        AppCoordinator::spawn(config_manager, compiler, supervisor, kernel, coord_config)
    }
}

#[derive(Clone)]
struct SharedSystemProxyBackend(Arc<dyn SystemProxyBackend>);

impl SystemProxyBackend for SharedSystemProxyBackend {
    fn get_current(&self) -> Result<SystemProxySnapshot, SysError> {
        self.0.get_current()
    }

    fn apply(&self, snapshot: &SystemProxySnapshot) -> Result<(), SysError> {
        self.0.apply(snapshot)
    }
}

fn is_dry_run_env() -> bool {
    std::env::var_os("VERVE_DRY_RUN_SYSPROXY")
        .is_some_and(|val| val == "1" || val.eq_ignore_ascii_case("true"))
}

fn default_system_proxy_backend() -> Arc<dyn SystemProxyBackend> {
    if is_dry_run_env() {
        Arc::new(InMemorySystemProxyBackend::default())
    } else {
        Arc::new(OsSystemProxyBackend)
    }
}

fn default_auto_launch_backend() -> Result<Arc<dyn AutoLaunchBackend>, CoreError> {
    if is_dry_run_env() {
        return Ok(Arc::new(InMemoryAutoLaunchBackend::default()));
    }
    let exe = std::env::current_exe().map_err(|err| CoreError::Sys(SysError::Io(err)))?;
    let backend = OsAutoLaunchBackend::new("Verve", &exe, &[])?;
    Ok(Arc::new(backend))
}

/// Single-writer CQRS orchestrator serializing configuration, compilation, and kernel control.
pub struct AppCoordinator<K: ProxyKernel> {
    config_manager: ConfigManager,
    state_store: StateStore,
    syncable: SyncableConfig,
    local: LocalMachineConfig,
    compiler: CompilerPipeline,
    supervisor: KernelSupervisor,
    kernel: Arc<K>,
    snapshots: Arc<SnapshotStore>,
    telemetry: TelemetryHub<K>,
    sysproxy_manager: SystemProxyManager<SharedSystemProxyBackend>,
    subscription_fetcher: Arc<dyn SubscriptionFetcher>,
    auto_launch_backend: Option<Arc<dyn AutoLaunchBackend>>,
    service_control_backend: Arc<dyn ServiceControlBackend>,
    config: CoordinatorConfig,
}

impl<K: ProxyKernel> AppCoordinator<K> {
    /// Initializes state from disk and spawns the single-writer coordinator loop on Tokio.
    pub fn spawn(
        config_manager: ConfigManager,
        compiler: CompilerPipeline,
        supervisor: KernelSupervisor,
        kernel: Arc<K>,
        config: CoordinatorConfig,
    ) -> Result<CoordinatorHandle<K>, CoreError> {
        let mut syncable = config_manager.load_syncable()?;
        let local = config_manager.load_local()?;
        let state_store = config_manager.open_state_store()?;
        let sysproxy_backend = config
            .system_proxy_backend
            .clone()
            .unwrap_or_else(default_system_proxy_backend);
        let sysproxy_manager = SystemProxyManager::with_backend(
            config_manager.runtime_dir(),
            SharedSystemProxyBackend(sysproxy_backend),
        );

        let subscription_fetcher = config
            .subscription_fetcher
            .clone()
            .unwrap_or_else(|| Arc::new(ReqwestSubscriptionFetcher::default()));
        let auto_launch_backend = config.auto_launch_backend.clone();
        let service_control_backend = config
            .service_control_backend
            .clone()
            .unwrap_or_else(|| Arc::new(OsServiceControlBackend));

        match sysproxy_manager.heal_if_crashed() {
            Ok(true) => {
                tracing::warn!(
                    "healed stale system proxy takeover marker from previous unclean exit"
                );
                if syncable.system_proxy_enabled {
                    syncable.system_proxy_enabled = false;
                    config_manager.save_syncable(&syncable)?;
                }
            }
            Ok(false) => {}
            Err(err) => {
                tracing::warn!(error = %err, "failed to heal stale system proxy marker on startup");
            }
        }
        let usages_vec = state_store.list_profile_usages()?;
        let usages: BTreeMap<String, _> = usages_vec
            .into_iter()
            .map(|usage| (usage.profile_id.clone(), usage))
            .collect();

        let active_profile_name = syncable.active_profile_id.as_deref().and_then(|active_id| {
            syncable
                .profiles
                .iter()
                .find(|entry| entry.id == active_id)
                .map(|entry| entry.name.clone())
        });

        let initial_snapshot = AppSnapshot {
            runtime: Arc::new(RuntimeSlice {
                revision: 0,
                proxy_mode: parse_proxy_mode(&syncable.proxy_mode),
                kernel_running: supervisor.is_running(),
                kernel_pid: supervisor.pid(),
                kernel_version: None,
                kernel_started_at_unix_secs: None,
                active_profile_id: syncable.active_profile_id.clone(),
                active_profile_name,
                mixed_port: syncable.mixed_port,
                system_proxy_enabled: syncable.system_proxy_enabled,
                tun_enabled: syncable.tun_enabled,
                control_endpoint: Some(config.control_endpoint.clone()),
                service_status: ServiceStatus::Unknown,
                last_error: None,
            }),
            profiles: Arc::new(ProfilesSlice {
                revision: 0,
                profiles: syncable.profiles.clone(),
                usages,
                active_profile_id: syncable.active_profile_id.clone(),
                global_merge_enabled: syncable.global_merge_enabled,
                global_script_enabled: syncable.global_script_enabled,
                last_compile_diagnostic: None,
            }),
            settings: Arc::new(SettingsSlice {
                revision: 0,
                syncable: syncable.clone(),
                local: local.clone(),
            }),
            ..Default::default()
        };

        let snapshots = Arc::new(SnapshotStore::new(initial_snapshot));
        let telemetry = TelemetryHub::with_capacities(
            Arc::clone(&kernel),
            Arc::clone(&snapshots),
            config.traffic_history_capacity,
            config.log_capacity,
        );

        let (tx, rx) = mpsc::channel(config.command_buffer_capacity.max(1));
        let bus = CommandBus::new(tx);

        let coordinator = Self {
            config_manager,
            state_store,
            syncable,
            local,
            compiler,
            supervisor,
            kernel: Arc::clone(&kernel),
            snapshots: Arc::clone(&snapshots),
            telemetry: telemetry.clone(),
            sysproxy_manager,
            subscription_fetcher,
            auto_launch_backend,
            service_control_backend,
            config,
        };

        let task = tokio::spawn(coordinator.run_loop(rx));

        Ok(CoordinatorHandle {
            bus,
            snapshots,
            telemetry,
            kernel,
            task: Some(task),
        })
    }

    async fn run_loop(mut self, mut rx: mpsc::Receiver<AppCommand>) {
        while let Some(command) = rx.recv().await {
            match command {
                AppCommand::ImportLocalProfile {
                    id,
                    name,
                    raw_yaml,
                    merge_yaml,
                    reply,
                } => {
                    let res = self.handle_import_local_profile(id, name, raw_yaml, merge_yaml);
                    send_reply("import_local_profile", reply, res);
                }
                AppCommand::ImportRemoteProfile { url, name, reply } => {
                    let res = self.handle_import_remote_profile(url, name).await;
                    send_reply("import_remote_profile", reply, res);
                }
                AppCommand::RefreshProfile { profile_id, reply } => {
                    let res = self.handle_refresh_profile(&profile_id).await;
                    send_reply("refresh_profile", reply, res);
                }
                AppCommand::DeleteProfile { profile_id, reply } => {
                    let res = self.handle_delete_profile(&profile_id).await;
                    send_reply("delete_profile", reply, res);
                }
                AppCommand::ActivateProfile { profile_id, reply } => {
                    let res = self.handle_activate_profile(&profile_id).await;
                    send_reply("activate_profile", reply, res);
                }
                AppCommand::StartKernel { reply } => {
                    let res = self.handle_start_kernel().await;
                    if let Err(ref err) = res {
                        let msg = err.to_string();
                        self.snapshots.publish_runtime(|r| r.last_error = Some(msg));
                    }
                    send_reply("start_kernel", reply, res);
                }
                AppCommand::StopKernel { reply } => {
                    let res = self.handle_stop_kernel().await;
                    send_reply("stop_kernel", reply, res);
                }
                AppCommand::RestartKernel { reply } => {
                    let res = self.handle_restart_kernel().await;
                    if let Err(ref err) = res {
                        let msg = err.to_string();
                        self.snapshots.publish_runtime(|r| r.last_error = Some(msg));
                    }
                    send_reply("restart_kernel", reply, res);
                }
                AppCommand::RefreshProxies { reply } => {
                    let res = self.refresh_proxies_internal().await;
                    send_reply("refresh_proxies", reply, res);
                }
                AppCommand::SelectProxy { group, node, reply } => {
                    let res = self.handle_select_proxy(&group, &node).await;
                    send_reply("select_proxy", reply, res);
                }
                AppCommand::TestDelay {
                    node,
                    url,
                    timeout,
                    reply,
                } => {
                    let res = self.handle_test_delay(&node, &url, timeout).await;
                    send_reply("test_delay", reply, res);
                }
                AppCommand::TestGroupDelay {
                    group,
                    url,
                    timeout,
                    reply,
                } => {
                    let res = self.handle_test_group_delay(&group, &url, timeout).await;
                    send_reply("test_group_delay", reply, res);
                }
                AppCommand::TestAllGroupsDelay {
                    url,
                    timeout,
                    reply,
                } => {
                    let res = self.handle_test_all_groups_delay(&url, timeout).await;
                    send_reply("test_all_groups_delay", reply, res);
                }
                AppCommand::SetProxyMode { mode, reply } => {
                    let res = self.handle_set_proxy_mode(mode).await;
                    send_reply("set_proxy_mode", reply, res);
                }
                AppCommand::SetSystemProxy { enabled, reply } => {
                    let res = self.handle_set_system_proxy(enabled);
                    if let Err(ref err) = res {
                        let msg = err.to_string();
                        self.snapshots.publish_runtime(|r| r.last_error = Some(msg));
                    }
                    send_reply("set_system_proxy", reply, res);
                }
                AppCommand::SetTunEnabled { enabled, reply } => {
                    let res = self.handle_set_tun_enabled(enabled).await;
                    if let Err(ref err) = res {
                        let msg = err.to_string();
                        self.snapshots.publish_runtime(|r| r.last_error = Some(msg));
                    }
                    send_reply("set_tun_enabled", reply, res);
                }
                AppCommand::UpdateNetworkSettings {
                    mixed_port,
                    allow_lan,
                    ipv6,
                    system_proxy_bypass,
                    reply,
                } => {
                    let res = self
                        .handle_update_network_settings(
                            mixed_port,
                            allow_lan,
                            ipv6,
                            system_proxy_bypass,
                        )
                        .await;
                    send_reply("update_network_settings", reply, res);
                }
                AppCommand::UpdateTunSettings {
                    tun,
                    tun_interface_name,
                    reply,
                } => {
                    let res = self
                        .handle_update_tun_settings(tun, tun_interface_name)
                        .await;
                    send_reply("update_tun_settings", reply, res);
                }
                AppCommand::SetThemeMode { mode, reply } => {
                    let res = self.handle_set_theme_mode(mode);
                    send_reply("set_theme_mode", reply, res);
                }
                AppCommand::SetThemeSeedColor { seed_argb, reply } => {
                    let res = self.handle_set_theme_seed_color(seed_argb);
                    send_reply("set_theme_seed_color", reply, res);
                }
                AppCommand::SetLocale { locale, reply } => {
                    let res = self.handle_set_locale(locale);
                    send_reply("set_locale", reply, res);
                }
                AppCommand::SetAutoLaunch { enabled, reply } => {
                    let res = self.handle_set_auto_launch(enabled);
                    send_reply("set_auto_launch", reply, res);
                }
                AppCommand::SetServiceMode { enabled, reply } => {
                    let res = self.handle_set_service_mode(enabled).await;
                    if let Err(ref err) = res {
                        let msg = err.to_string();
                        self.snapshots.publish_runtime(|r| r.last_error = Some(msg));
                    }
                    send_reply("set_service_mode", reply, res);
                }
                AppCommand::ServiceAction { action, reply } => {
                    let res = self.handle_service_action(action).await;
                    send_reply("service_action", reply, res);
                }
                AppCommand::RefreshRules { reply } => {
                    let res = self.handle_refresh_rules().await;
                    send_reply("refresh_rules", reply, res);
                }
                AppCommand::Shutdown { reply } => {
                    let res = self.handle_shutdown().await;
                    send_reply("shutdown", reply, res);
                    return;
                }
            }
        }

        // Channel closed without explicit Shutdown command: clean up deterministically.
        let _ = self.handle_shutdown().await;
    }

    fn handle_import_local_profile(
        &mut self,
        id: String,
        name: String,
        raw_yaml: String,
        merge_yaml: Option<String>,
    ) -> Result<ProfileEntry, CoreError> {
        let file_name = format!("{id}.yaml");
        let merge_file_name = merge_yaml.as_ref().map(|_| format!("{id}.merge.yaml"));

        // Validate paths before writing files to disk.
        let _ = self.config_manager.profile_path(&file_name)?;
        if let Some(ref mf) = merge_file_name {
            let _ = self.config_manager.override_path(mf)?;
        }

        let existing_script = self
            .syncable
            .profiles
            .iter()
            .find(|p| p.id == id)
            .and_then(|p| p.script_file_name.clone());

        let entry = ProfileEntry {
            id: id.clone(),
            name,
            source: ProfileSource::Local,
            file_name: file_name.clone(),
            merge_file_name: merge_file_name.clone(),
            script_file_name: existing_script,
        };

        let mut candidate_syncable = self.syncable.clone();
        if let Some(slot) = candidate_syncable.profiles.iter_mut().find(|p| p.id == id) {
            *slot = entry.clone();
        } else {
            candidate_syncable.profiles.push(entry.clone());
        }
        candidate_syncable.normalize_and_validate()?;

        self.config_manager
            .write_profile_yaml(&file_name, &raw_yaml)?;
        if let (Some(mf), Some(merge)) = (merge_file_name.as_deref(), merge_yaml.as_deref()) {
            self.config_manager.write_override_file(mf, merge)?;
        }

        self.syncable = candidate_syncable;
        self.config_manager.save_syncable(&self.syncable)?;

        self.snapshots.publish_profiles(|profiles| {
            profiles.profiles = self.syncable.profiles.clone();
        });
        self.snapshots.publish_settings(|settings| {
            settings.syncable = self.syncable.clone();
        });

        Ok(entry)
    }

    async fn handle_import_remote_profile(
        &mut self,
        url: String,
        name: Option<String>,
    ) -> Result<ProfileEntry, CoreError> {
        let trimmed_url = url.trim();
        if trimmed_url.is_empty() {
            return Err(CoreError::InvalidInput(
                "subscription URL must not be empty".into(),
            ));
        }

        let fetched = self.subscription_fetcher.fetch(trimmed_url).await?;
        validate_profile_yaml(&fetched.raw_yaml)?;

        let resolved_name = name
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty())
            .or(fetched.suggested_name)
            .or_else(|| infer_profile_name(trimmed_url, None, None))
            .unwrap_or_else(|| "Subscription".to_owned());

        let existing_ids: Vec<&str> = self
            .syncable
            .profiles
            .iter()
            .map(|entry| entry.id.as_str())
            .collect();
        let id = generate_profile_id(&resolved_name, &existing_ids);
        let file_name = format!("{id}.yaml");
        let _ = self.config_manager.profile_path(&file_name)?;

        let entry = ProfileEntry {
            id: id.clone(),
            name: resolved_name,
            source: ProfileSource::Remote {
                url: trimmed_url.to_owned(),
                update_interval_mins: None,
            },
            file_name: file_name.clone(),
            merge_file_name: None,
            script_file_name: None,
        };

        let mut candidate_syncable = self.syncable.clone();
        candidate_syncable.profiles.push(entry.clone());
        candidate_syncable.normalize_and_validate()?;

        self.config_manager
            .write_profile_yaml(&file_name, &fetched.raw_yaml)?;

        self.syncable = candidate_syncable;
        self.config_manager.save_syncable(&self.syncable)?;

        let usage_record = if let Some(usage) = fetched.usage {
            let record = ProfileUsage {
                profile_id: id.clone(),
                upload_bytes: usage.upload_bytes,
                download_bytes: usage.download_bytes,
                total_bytes: usage.total_bytes,
                expire_unix_secs: usage.expire_unix_secs,
                updated_at_unix_secs: now_unix_secs(),
            };
            self.state_store.upsert_profile_usage(&record)?;
            Some(record)
        } else {
            None
        };

        self.snapshots.publish_profiles(|profiles| {
            profiles.profiles = self.syncable.profiles.clone();
            if let Some(record) = usage_record {
                profiles.usages.insert(record.profile_id.clone(), record);
            }
        });
        self.snapshots.publish_settings(|settings| {
            settings.syncable = self.syncable.clone();
        });

        Ok(entry)
    }

    async fn handle_refresh_profile(
        &mut self,
        profile_id: &str,
    ) -> Result<ProfileEntry, CoreError> {
        let profile = self
            .syncable
            .profiles
            .iter()
            .find(|entry| entry.id == profile_id)
            .cloned()
            .ok_or_else(|| CoreError::ProfileNotFound(profile_id.to_owned()))?;

        let url = match &profile.source {
            ProfileSource::Remote { url, .. } => url.clone(),
            ProfileSource::Local => {
                return Err(CoreError::InvalidInput(format!(
                    "profile `{profile_id}` is a local profile and has no remote subscription URL"
                )));
            }
        };

        // Fetch and validate YAML BEFORE touching the existing profile file on disk.
        let fetched = self.subscription_fetcher.fetch(&url).await?;
        validate_profile_yaml(&fetched.raw_yaml)?;

        self.config_manager
            .write_profile_yaml(&profile.file_name, &fetched.raw_yaml)?;

        let usage_record = if let Some(usage) = fetched.usage {
            let record = ProfileUsage {
                profile_id: profile.id.clone(),
                upload_bytes: usage.upload_bytes,
                download_bytes: usage.download_bytes,
                total_bytes: usage.total_bytes,
                expire_unix_secs: usage.expire_unix_secs,
                updated_at_unix_secs: now_unix_secs(),
            };
            self.state_store.upsert_profile_usage(&record)?;
            Some(record)
        } else {
            None
        };

        let is_active = self.syncable.active_profile_id.as_deref() == Some(profile_id);
        if is_active && self.supervisor.is_running() {
            let (_profile, committed) = self.compile_and_commit_profile(profile_id).await?;
            self.kernel.reload_config(&committed.active_path).await?;
            self.apply_saved_selections(profile_id).await;
            self.refresh_proxies_internal().await?;
        }

        self.snapshots.publish_profiles(|profiles| {
            if let Some(record) = usage_record {
                profiles.usages.insert(record.profile_id.clone(), record);
            }
            if is_active {
                profiles.last_compile_diagnostic = None;
            }
        });

        Ok(profile)
    }

    async fn handle_delete_profile(&mut self, profile_id: &str) -> Result<(), CoreError> {
        let index = self
            .syncable
            .profiles
            .iter()
            .position(|entry| entry.id == profile_id)
            .ok_or_else(|| CoreError::ProfileNotFound(profile_id.to_owned()))?;

        let was_active = self.syncable.active_profile_id.as_deref() == Some(profile_id);
        if was_active && self.supervisor.is_running() {
            self.handle_stop_kernel().await?;
        }

        let entry = self.syncable.profiles.remove(index);
        if was_active {
            self.syncable.active_profile_id = None;
            let active_yaml = self.config_manager.runtime_dir().join("active.yaml");
            let _ = std::fs::remove_file(active_yaml);
        }

        self.config_manager.save_syncable(&self.syncable)?;
        self.config_manager.remove_profile_files(&entry)?;
        self.state_store.remove_profile_metadata(profile_id)?;

        self.snapshots.publish_profiles(|profiles| {
            profiles.profiles = self.syncable.profiles.clone();
            profiles.usages.remove(profile_id);
            profiles.active_profile_id = self.syncable.active_profile_id.clone();
            if was_active {
                profiles.last_compile_diagnostic = None;
            }
        });
        self.snapshots.publish_settings(|settings| {
            settings.syncable = self.syncable.clone();
        });
        if was_active {
            self.snapshots.publish_runtime(|runtime| {
                runtime.active_profile_id = None;
                runtime.active_profile_name = None;
            });
        }

        Ok(())
    }

    async fn handle_activate_profile(
        &mut self,
        profile_id: &str,
    ) -> Result<CommittedConfig, CoreError> {
        let (profile, committed) = self.compile_and_commit_profile(profile_id).await?;

        self.syncable.active_profile_id = Some(profile.id.clone());
        self.config_manager.save_syncable(&self.syncable)?;

        self.snapshots.publish_profiles(|profiles| {
            profiles.active_profile_id = Some(profile.id.clone());
            profiles.last_compile_diagnostic = None;
        });
        self.snapshots.publish_settings(|settings| {
            settings.syncable = self.syncable.clone();
        });
        self.snapshots.publish_runtime(|runtime| {
            runtime.active_profile_id = Some(profile.id.clone());
            runtime.active_profile_name = Some(profile.name.clone());
            runtime.mixed_port = self.syncable.mixed_port;
            runtime.proxy_mode = parse_proxy_mode(&self.syncable.proxy_mode);
            runtime.system_proxy_enabled = self.syncable.system_proxy_enabled;
            runtime.tun_enabled = self.syncable.tun_enabled;
            runtime.last_error = None;
        });

        if self.supervisor.is_running() {
            self.kernel.reload_config(&committed.active_path).await?;
            self.apply_saved_selections(&profile.id).await;
            self.refresh_proxies_internal().await?;
        }

        Ok(committed)
    }

    async fn compile_and_commit_profile(
        &mut self,
        profile_id: &str,
    ) -> Result<(ProfileEntry, CommittedConfig), CoreError> {
        let profile = self
            .syncable
            .profiles
            .iter()
            .find(|entry| entry.id == profile_id)
            .cloned()
            .ok_or_else(|| CoreError::ProfileNotFound(profile_id.to_owned()))?;

        let raw_yaml = self.config_manager.read_profile_yaml(&profile.file_name)?;
        let profile_merge_yaml = profile
            .merge_file_name
            .as_deref()
            .map(|file| self.config_manager.read_override_file(file))
            .transpose()?;
        let profile_script = profile
            .script_file_name
            .as_deref()
            .map(|file| self.config_manager.read_override_file(file))
            .transpose()?;

        let global_merge_yaml = if self.syncable.global_merge_enabled {
            read_optional_override(&self.config_manager, "global.merge.yaml")?
        } else {
            None
        };
        let global_script = if self.syncable.global_script_enabled {
            read_optional_override(&self.config_manager, "global.script.js")?
        } else {
            None
        };

        let ctx = CompileContext {
            profile_name: profile.name.clone(),
            profile_merge_yaml,
            profile_script,
            global_merge_yaml,
            global_script,
            quick_rules: self.syncable.quick_rules.clone(),
            runtime: self.build_runtime_injection_config(),
        };

        let runtime_dir = self.config_manager.runtime_dir();
        let kernel = Arc::clone(&self.kernel);
        match self
            .compiler
            .compile_and_commit(&raw_yaml, &ctx, &runtime_dir, |candidate| {
                let candidate_path = candidate.to_path_buf();
                async move { kernel.validate_config(&candidate_path).await }
            })
            .await
        {
            Ok(committed) => Ok((profile, committed)),
            Err(diag) => {
                self.snapshots.publish_profiles(|profiles| {
                    profiles.last_compile_diagnostic = Some(diag.clone());
                });
                Err(CoreError::Compile(diag))
            }
        }
    }

    async fn handle_start_kernel(&mut self) -> Result<u32, CoreError> {
        if let Some(pid) = self.supervisor.pid() {
            return Err(CoreError::Kernel(KernelError::AlreadyRunning { pid }));
        }

        let local = self.config_manager.load_local()?;
        self.local = local.clone();
        self.sync_supervisor_kind(local.service_mode_enabled);

        let active_path = self.config_manager.runtime_dir().join("active.yaml");
        if !active_path.exists() {
            let active_id = self
                .syncable
                .active_profile_id
                .clone()
                .ok_or(CoreError::NoActiveProfile)?;
            self.handle_activate_profile(&active_id).await?;
        }

        let bin_path = self
            .resolve_kernel_binary()
            .ok_or(CoreError::KernelBinaryNotFound)?;

        self.remove_stale_unix_socket();

        let runtime_dir = self.config_manager.runtime_dir();
        let pid = self
            .supervisor
            .start(&bin_path, &runtime_dir, &active_path)
            .await?;

        if let Err(err) = self.wait_for_control_plane().await {
            let _ = self.stop_supervisor_best_effort().await;
            self.remove_stale_unix_socket();
            self.snapshots.publish_runtime(|runtime| {
                runtime.kernel_running = false;
                runtime.kernel_pid = None;
                runtime.kernel_version = None;
                runtime.kernel_started_at_unix_secs = None;
                runtime.last_error = Some(err.to_string());
            });
            return Err(err);
        }

        let kernel_version = self.kernel.fetch_version().await.ok().flatten();
        let started_at = now_unix_secs();

        if let Some(active_id) = self.syncable.active_profile_id.clone() {
            self.apply_saved_selections(&active_id).await;
        }
        self.refresh_proxies_internal().await?;

        self.snapshots.publish_runtime(|runtime| {
            runtime.kernel_running = true;
            runtime.kernel_pid = Some(pid);
            runtime.kernel_version = kernel_version;
            runtime.kernel_started_at_unix_secs = Some(started_at);
            runtime.control_endpoint = Some(self.config.control_endpoint.clone());
            runtime.last_error = None;
        });

        Ok(pid)
    }

    async fn handle_stop_kernel(&mut self) -> Result<(), CoreError> {
        self.stop_supervisor_checked().await?;
        self.remove_stale_unix_socket();
        self.snapshots.publish_runtime(|runtime| {
            runtime.kernel_running = false;
            runtime.kernel_pid = None;
            runtime.kernel_version = None;
            runtime.kernel_started_at_unix_secs = None;
        });
        Ok(())
    }

    async fn handle_restart_kernel(&mut self) -> Result<u32, CoreError> {
        if self.supervisor.is_running() {
            self.handle_stop_kernel().await?;
        }
        self.handle_start_kernel().await
    }

    async fn refresh_proxies_internal(&mut self) -> Result<ProxyTopology, CoreError> {
        let mut topology = self.kernel.fetch_proxies().await?;
        let delay_cache = self.state_store.get_delay_cache()?;
        overlay_cached_delays(&mut topology, &delay_cache);

        self.snapshots.publish_proxies(|proxies| {
            proxies.groups = topology.groups.clone();
            proxies.nodes = topology.nodes.clone();
        });

        Ok(topology)
    }

    async fn handle_select_proxy(&mut self, group: &str, node: &str) -> Result<(), CoreError> {
        self.kernel.select_proxy(group, node).await?;
        if let Some(active_id) = self.syncable.active_profile_id.as_deref() {
            self.state_store
                .set_proxy_selection(active_id, group, node)?;
        }
        self.refresh_proxies_internal().await?;
        Ok(())
    }

    async fn handle_test_delay(
        &mut self,
        node: &str,
        url: &str,
        timeout: Duration,
    ) -> Result<u16, CoreError> {
        let delay = self.kernel.test_delay(node, url, timeout).await?;
        self.state_store
            .set_delay_ms(node, delay, now_unix_secs())?;

        let mut topology = self.kernel.fetch_proxies().await?;
        let delay_cache = self.state_store.get_delay_cache()?;
        overlay_cached_delays(&mut topology, &delay_cache);
        if let Some(entry) = topology.nodes.get_mut(node) {
            entry.latest_delay_ms = Some(delay);
        }

        self.snapshots.publish_proxies(|proxies| {
            proxies.groups = topology.groups;
            proxies.nodes = topology.nodes;
        });

        Ok(delay)
    }

    async fn handle_test_group_delay(
        &mut self,
        group: &str,
        url: &str,
        timeout: Duration,
    ) -> Result<BTreeMap<String, Option<u16>>, CoreError> {
        let topology = self.kernel.fetch_proxies().await?;
        let target_group = topology
            .groups
            .iter()
            .find(|g| g.name == group)
            .ok_or_else(|| CoreError::InvalidInput(format!("proxy group `{group}` not found")))?;

        let group_names: HashSet<&str> = topology.groups.iter().map(|g| g.name.as_str()).collect();
        let mut seen = HashSet::new();
        let mut targets = Vec::new();
        for member in &target_group.all {
            if is_builtin_proxy_name(member) || group_names.contains(member.as_str()) {
                continue;
            }
            if seen.insert(member.as_str()) {
                targets.push(member.clone());
            }
        }

        self.run_concurrent_delay_probes(topology, targets, url, timeout)
            .await
    }

    async fn handle_test_all_groups_delay(
        &mut self,
        url: &str,
        timeout: Duration,
    ) -> Result<BTreeMap<String, Option<u16>>, CoreError> {
        let topology = self.kernel.fetch_proxies().await?;
        let group_names: HashSet<&str> = topology.groups.iter().map(|g| g.name.as_str()).collect();
        let mut seen = HashSet::new();
        let mut targets = Vec::new();

        for group in topology.groups.iter().filter(|g| !g.hidden) {
            for member in &group.all {
                if is_builtin_proxy_name(member) || group_names.contains(member.as_str()) {
                    continue;
                }
                if seen.insert(member.as_str()) {
                    targets.push(member.clone());
                }
            }
        }

        self.run_concurrent_delay_probes(topology, targets, url, timeout)
            .await
    }

    async fn run_concurrent_delay_probes(
        &mut self,
        mut topology: ProxyTopology,
        targets: Vec<String>,
        url: &str,
        timeout: Duration,
    ) -> Result<BTreeMap<String, Option<u16>>, CoreError> {
        let kernel = Arc::clone(&self.kernel);
        let probe_url = url.to_owned();

        let measured: Vec<(String, Option<u16>)> = stream::iter(targets)
            .map(|node| {
                let kernel = Arc::clone(&kernel);
                let probe_url = probe_url.clone();
                async move {
                    let delay = kernel.test_delay(&node, &probe_url, timeout).await.ok();
                    (node, delay)
                }
            })
            .buffer_unordered(MAX_CONCURRENT_DELAY_TESTS)
            .collect()
            .await;

        let now = now_unix_secs();
        let mut results = BTreeMap::new();
        for (node, delay) in measured {
            if let Some(ms) = delay {
                self.state_store.set_delay_ms(&node, ms, now)?;
            }
            results.insert(node, delay);
        }

        let delay_cache = self.state_store.get_delay_cache()?;
        overlay_cached_delays(&mut topology, &delay_cache);
        for (node, delay) in &results {
            if let Some(entry) = topology.nodes.get_mut(node) {
                entry.latest_delay_ms = *delay;
            }
        }

        self.snapshots.publish_proxies(|proxies| {
            proxies.groups = topology.groups;
            proxies.nodes = topology.nodes;
        });

        Ok(results)
    }

    async fn handle_set_proxy_mode(&mut self, mode: ProxyMode) -> Result<(), CoreError> {
        self.syncable.proxy_mode = proxy_mode_to_str(mode).to_owned();
        self.config_manager.save_syncable(&self.syncable)?;

        self.snapshots.publish_runtime(|runtime| {
            runtime.proxy_mode = mode;
        });
        self.snapshots.publish_settings(|settings| {
            settings.syncable = self.syncable.clone();
        });

        if self.supervisor.is_running()
            && let Some(active_id) = self.syncable.active_profile_id.clone()
        {
            let (_profile, committed) = self.compile_and_commit_profile(&active_id).await?;
            self.kernel.reload_config(&committed.active_path).await?;
            self.apply_saved_selections(&active_id).await;
            self.refresh_proxies_internal().await?;
        }

        Ok(())
    }

    fn handle_set_system_proxy(&mut self, enabled: bool) -> Result<(), CoreError> {
        if enabled {
            self.sysproxy_manager.enable(
                "127.0.0.1",
                self.syncable.mixed_port,
                &self.syncable.system_proxy_bypass,
            )?;
        } else {
            self.sysproxy_manager.disable()?;
        }

        self.syncable.system_proxy_enabled = enabled;
        self.config_manager.save_syncable(&self.syncable)?;

        self.snapshots.publish_runtime(|runtime| {
            runtime.system_proxy_enabled = enabled;
            runtime.last_error = None;
        });
        self.snapshots.publish_settings(|settings| {
            settings.syncable = self.syncable.clone();
        });

        Ok(())
    }

    async fn handle_set_tun_enabled(&mut self, enabled: bool) -> Result<(), CoreError> {
        self.syncable.tun_enabled = enabled;
        self.config_manager.save_syncable(&self.syncable)?;

        self.snapshots.publish_runtime(|runtime| {
            runtime.tun_enabled = enabled;
            runtime.last_error = None;
        });
        self.snapshots.publish_settings(|settings| {
            settings.syncable = self.syncable.clone();
        });

        if let Some(active_id) = self.syncable.active_profile_id.clone() {
            let (_profile, committed) = self.compile_and_commit_profile(&active_id).await?;
            let local = self.config_manager.load_local()?;
            let want_service =
                local.service_mode_enabled && self.config.service_ipc_client.is_some();
            let is_service = matches!(self.supervisor, KernelSupervisor::Service(_));

            if self.supervisor.is_running() {
                if want_service != is_service {
                    self.stop_supervisor_best_effort().await?;
                    self.remove_stale_unix_socket();
                    self.snapshots.publish_runtime(|runtime| {
                        runtime.kernel_running = false;
                        runtime.kernel_pid = None;
                        runtime.kernel_version = None;
                        runtime.kernel_started_at_unix_secs = None;
                    });
                    self.sync_supervisor_kind(local.service_mode_enabled);
                    self.handle_start_kernel().await?;
                } else {
                    self.kernel.reload_config(&committed.active_path).await?;
                    self.apply_saved_selections(&active_id).await;
                    self.refresh_proxies_internal().await?;
                }
            } else {
                self.sync_supervisor_kind(local.service_mode_enabled);
            }
        }

        Ok(())
    }

    async fn handle_update_network_settings(
        &mut self,
        mixed_port: u16,
        allow_lan: bool,
        ipv6: bool,
        system_proxy_bypass: Vec<String>,
    ) -> Result<(), CoreError> {
        let mut candidate = self.syncable.clone();
        candidate.mixed_port = mixed_port;
        candidate.allow_lan = allow_lan;
        candidate.ipv6 = ipv6;
        candidate.system_proxy_bypass = system_proxy_bypass
            .into_iter()
            .map(|entry| entry.trim().to_owned())
            .filter(|entry| !entry.is_empty())
            .collect();
        candidate.normalize_and_validate()?;

        self.syncable = candidate;
        self.config_manager.save_syncable(&self.syncable)?;

        self.snapshots.publish_settings(|settings| {
            settings.syncable = self.syncable.clone();
        });
        self.snapshots.publish_runtime(|runtime| {
            runtime.mixed_port = self.syncable.mixed_port;
        });

        if self.syncable.system_proxy_enabled {
            self.sysproxy_manager.enable(
                "127.0.0.1",
                self.syncable.mixed_port,
                &self.syncable.system_proxy_bypass,
            )?;
        }

        if self.supervisor.is_running()
            && let Some(active_id) = self.syncable.active_profile_id.clone()
        {
            let (_profile, committed) = self.compile_and_commit_profile(&active_id).await?;
            self.kernel.reload_config(&committed.active_path).await?;
            self.apply_saved_selections(&active_id).await;
            self.refresh_proxies_internal().await?;
        }

        Ok(())
    }

    async fn handle_update_tun_settings(
        &mut self,
        tun: TunConfig,
        tun_interface_name: Option<String>,
    ) -> Result<(), CoreError> {
        let mut candidate_syncable = self.syncable.clone();
        candidate_syncable.tun = tun;
        candidate_syncable.normalize_and_validate()?;

        let mut candidate_local = self.local.clone();
        candidate_local.tun_interface_name = tun_interface_name;
        candidate_local.normalize_and_validate()?;

        self.syncable = candidate_syncable;
        self.local = candidate_local;
        self.config_manager.save_syncable(&self.syncable)?;
        self.config_manager.save_local(&self.local)?;

        self.snapshots.publish_settings(|settings| {
            settings.syncable = self.syncable.clone();
            settings.local = self.local.clone();
        });

        if self.syncable.tun_enabled
            && self.supervisor.is_running()
            && let Some(active_id) = self.syncable.active_profile_id.clone()
        {
            let (_profile, committed) = self.compile_and_commit_profile(&active_id).await?;
            self.kernel.reload_config(&committed.active_path).await?;
            self.apply_saved_selections(&active_id).await;
            self.refresh_proxies_internal().await?;
        }

        Ok(())
    }

    fn handle_set_theme_mode(&mut self, mode: String) -> Result<(), CoreError> {
        let normalized = mode.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "light" | "dark" | "system" => {}
            _ => {
                return Err(CoreError::Config(ConfigError::InvalidField {
                    field: "theme_mode",
                    value: mode,
                    reason: "expected one of \"light\", \"dark\", or \"system\"".into(),
                }));
            }
        }

        self.syncable.theme_mode = normalized;
        self.config_manager.save_syncable(&self.syncable)?;
        self.snapshots.publish_settings(|settings| {
            settings.syncable = self.syncable.clone();
        });
        Ok(())
    }

    fn handle_set_theme_seed_color(&mut self, seed_argb: Option<u32>) -> Result<(), CoreError> {
        self.syncable.theme_seed_color = seed_argb;
        self.config_manager.save_syncable(&self.syncable)?;
        self.snapshots.publish_settings(|settings| {
            settings.syncable = self.syncable.clone();
        });
        Ok(())
    }

    fn handle_set_locale(&mut self, locale: String) -> Result<(), CoreError> {
        let normalized = verve_config::normalize_locale(&locale)?;
        self.syncable.locale = normalized;
        self.config_manager.save_syncable(&self.syncable)?;
        self.snapshots.publish_settings(|settings| {
            settings.syncable = self.syncable.clone();
        });
        Ok(())
    }

    fn handle_set_auto_launch(&mut self, enabled: bool) -> Result<(), CoreError> {
        let backend = match &self.auto_launch_backend {
            Some(backend) => Arc::clone(backend),
            None => {
                let resolved = default_auto_launch_backend()?;
                self.auto_launch_backend = Some(Arc::clone(&resolved));
                resolved
            }
        };
        backend.set_enabled(enabled)?;

        self.local.auto_launch = enabled;
        self.config_manager.save_local(&self.local)?;
        self.snapshots.publish_settings(|settings| {
            settings.local = self.local.clone();
        });
        Ok(())
    }

    async fn handle_set_service_mode(&mut self, enabled: bool) -> Result<(), CoreError> {
        self.local.service_mode_enabled = enabled;
        self.config_manager.save_local(&self.local)?;
        self.snapshots.publish_settings(|settings| {
            settings.local = self.local.clone();
        });

        let want_service = enabled && self.config.service_ipc_client.is_some();
        let is_service = matches!(self.supervisor, KernelSupervisor::Service(_));
        if self.supervisor.is_running() {
            if want_service != is_service {
                self.handle_stop_kernel().await?;
                self.sync_supervisor_kind(enabled);
                self.handle_start_kernel().await?;
            }
        } else {
            self.sync_supervisor_kind(enabled);
        }

        Ok(())
    }

    async fn handle_service_action(
        &mut self,
        action: ServiceControlAction,
    ) -> Result<ServiceStatus, CoreError> {
        match action {
            ServiceControlAction::CheckStatus => {}
            ServiceControlAction::Install => {
                let mut trusted_dirs = Vec::new();
                if let Some(bin) = self.resolve_kernel_binary()
                    && let Some(parent) = bin.parent()
                {
                    trusted_dirs.push(parent.to_path_buf());
                }
                let root = self.config_manager.root_dir().to_path_buf();
                if !trusted_dirs.contains(&root) {
                    trusted_dirs.push(root);
                }
                self.service_control_backend
                    .install_service(&trusted_dirs)
                    .await?;
            }
            ServiceControlAction::Uninstall => {
                self.service_control_backend.uninstall_service().await?;
            }
        }

        let status = probe_service_status(self.config.service_ipc_client.clone()).await;
        self.snapshots.publish_runtime(|runtime| {
            runtime.service_status = status.clone();
        });
        Ok(status)
    }

    fn sync_supervisor_kind(&mut self, service_mode_enabled: bool) {
        if self.supervisor.is_running() {
            return;
        }
        let want_service = service_mode_enabled && self.config.service_ipc_client.is_some();
        let is_service = matches!(self.supervisor, KernelSupervisor::Service(_));
        if want_service == is_service {
            return;
        }

        self.supervisor = if want_service && let Some(ref client) = self.config.service_ipc_client {
            KernelSupervisor::Service(DaemonIpcSupervisor::new(
                Arc::clone(client),
                "mihomo",
                self.config.control_endpoint.as_str_lossy().into_owned(),
            ))
        } else {
            KernelSupervisor::Direct(DirectProcessSupervisor::new())
        };
    }

    async fn handle_refresh_rules(&mut self) -> Result<RuleSnapshot, CoreError> {
        let snapshot = self.kernel.fetch_rules().await?;
        self.snapshots.publish_activity(|activity| {
            activity.rules = snapshot.rules.clone();
            activity.rule_providers = snapshot.providers.clone();
        });
        Ok(snapshot)
    }

    async fn handle_shutdown(&mut self) -> Result<(), CoreError> {
        self.telemetry.shutdown().await;
        if self.syncable.system_proxy_enabled || self.sysproxy_manager.marker_path().exists() {
            if let Err(err) = self.sysproxy_manager.disable() {
                tracing::warn!(error = %err, "failed to disable system proxy during shutdown");
            }
            if self.syncable.system_proxy_enabled {
                self.syncable.system_proxy_enabled = false;
                if let Err(err) = self.config_manager.save_syncable(&self.syncable) {
                    tracing::warn!(error = %err, "failed to persist system proxy reset during shutdown");
                }
            }
        }
        let stop_res = self.stop_supervisor_best_effort().await;
        self.remove_stale_unix_socket();
        self.snapshots.publish_runtime(|runtime| {
            runtime.kernel_running = false;
            runtime.kernel_pid = None;
            runtime.kernel_version = None;
            runtime.kernel_started_at_unix_secs = None;
            runtime.system_proxy_enabled = false;
        });
        stop_res
    }

    async fn wait_for_control_plane(&mut self) -> Result<ProxyTopology, CoreError> {
        let deadline = tokio::time::Instant::now() + self.config.startup_timeout;
        let mut backoff = Duration::from_millis(25);

        loop {
            if !self.supervisor.is_running() {
                let stderr = self.supervisor.stderr_tail(10);
                let detail = if stderr.is_empty() {
                    "kernel process exited unexpectedly during startup".to_owned()
                } else {
                    format!(
                        "kernel process exited during startup: {}",
                        stderr.join(" | ")
                    )
                };
                return Err(CoreError::Kernel(KernelError::Protocol(detail)));
            }

            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(CoreError::Kernel(KernelError::Timeout));
            }
            let remaining = deadline - now;

            match tokio::time::timeout(remaining, self.kernel.fetch_proxies()).await {
                Ok(Ok(topology)) => return Ok(topology),
                Ok(Err(err)) => {
                    tracing::debug!(error = %err, "kernel control plane not ready yet");
                }
                Err(_) => {
                    return Err(CoreError::Kernel(KernelError::Timeout));
                }
            }

            let now_after = tokio::time::Instant::now();
            if now_after >= deadline {
                return Err(CoreError::Kernel(KernelError::Timeout));
            }
            let sleep_for = backoff.min(deadline - now_after);
            tokio::time::sleep(sleep_for).await;
            backoff = (backoff * 2).min(Duration::from_millis(150));
        }
    }

    async fn apply_saved_selections(&mut self, profile_id: &str) {
        let selections = match self.state_store.get_proxy_selections(profile_id) {
            Ok(selections) => selections,
            Err(err) => {
                tracing::warn!(profile_id, error = %err, "failed to load saved proxy selections");
                return;
            }
        };

        for (group, node) in selections {
            if let Err(err) = self.kernel.select_proxy(&group, &node).await {
                tracing::warn!(
                    profile_id,
                    group = %group,
                    node = %node,
                    error = %err,
                    "failed to restore saved proxy selection"
                );
            }
        }
    }

    fn resolve_kernel_binary(&self) -> Option<PathBuf> {
        if let Some(ref explicit) = self.config.kernel_binary_path
            && explicit.is_file()
        {
            return Some(explicit.clone());
        }
        let configured = self
            .config_manager
            .load_local()
            .ok()
            .and_then(|local| local.mihomo_binary_path);
        discover_mihomo_binary(configured.as_deref())
    }

    fn build_runtime_injection_config(&self) -> RuntimeInjectionConfig {
        RuntimeInjectionConfig {
            mixed_port: self.syncable.mixed_port,
            allow_lan: self.syncable.allow_lan,
            ipv6: self.syncable.ipv6,
            mode: parse_proxy_mode(&self.syncable.proxy_mode),
            log_level: parse_log_level(&self.syncable.log_level),
            control_endpoint: self.config.control_endpoint.clone(),
            tun_enabled: self.syncable.tun_enabled,
            tun: TunInjectionConfig {
                stack: self.syncable.tun.stack.clone(),
                auto_route: self.syncable.tun.auto_route,
                strict_route: self.syncable.tun.strict_route,
                dns_hijack: self.syncable.tun.dns_hijack.clone(),
                mtu: self.syncable.tun.mtu,
                route_exclude_address: self.syncable.tun.route_exclude_address.clone(),
                interface_name: self.local.tun_interface_name.clone(),
            },
        }
    }

    async fn stop_supervisor_checked(&mut self) -> Result<(), CoreError> {
        match &self.supervisor {
            KernelSupervisor::Direct(_) => Ok(self.supervisor.stop().await?),
            KernelSupervisor::Service(_) if self.supervisor.is_running() => {
                Ok(self.supervisor.stop().await?)
            }
            KernelSupervisor::Service(_) => Ok(()),
        }
    }

    async fn stop_supervisor_best_effort(&mut self) -> Result<(), CoreError> {
        match &self.supervisor {
            KernelSupervisor::Direct(_) => Ok(self.supervisor.stop().await?),
            KernelSupervisor::Service(_) if self.supervisor.is_running() => {
                Ok(self.supervisor.stop().await?)
            }
            KernelSupervisor::Service(_) => Ok(()),
        }
    }

    fn remove_stale_unix_socket(&self) {
        #[cfg(unix)]
        if let ControlEndpoint::UnixSocket(ref path) = self.config.control_endpoint {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn read_optional_override(
    manager: &ConfigManager,
    file_name: &str,
) -> Result<Option<String>, CoreError> {
    match manager.read_override_file(file_name) {
        Ok(content) => Ok(Some(content)),
        Err(verve_config::ConfigError::Io(err)) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(CoreError::Config(err)),
    }
}

fn overlay_cached_delays(topology: &mut ProxyTopology, delay_cache: &BTreeMap<String, u16>) {
    for (name, node) in &mut topology.nodes {
        if node.latest_delay_ms.is_none()
            && let Some(&cached) = delay_cache.get(name)
        {
            node.latest_delay_ms = Some(cached);
        }
    }
}

fn is_builtin_proxy_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_uppercase().as_str(),
        "DIRECT" | "REJECT" | "REJECT-DROP" | "PASS" | "COMPATIBLE"
    )
}

async fn probe_service_status(client: Option<Arc<dyn ServiceIpcClient>>) -> ServiceStatus {
    match client {
        Some(client) => match client.query_remote_status().await {
            Ok(remote) => ServiceStatus::Available {
                running_pid: if remote.running { remote.pid } else { None },
            },
            Err(err) => ServiceStatus::Unavailable {
                reason: Some(err.to_string()),
            },
        },
        None => ServiceStatus::Unavailable {
            reason: Some("service IPC client is not configured".into()),
        },
    }
}

fn parse_proxy_mode(raw: &str) -> ProxyMode {
    match raw.trim().to_ascii_lowercase().as_str() {
        "global" => ProxyMode::Global,
        "direct" => ProxyMode::Direct,
        _ => ProxyMode::Rule,
    }
}

fn proxy_mode_to_str(mode: ProxyMode) -> &'static str {
    match mode {
        ProxyMode::Rule => "rule",
        ProxyMode::Global => "global",
        ProxyMode::Direct => "direct",
    }
}

fn parse_log_level(raw: &str) -> LogLevel {
    match raw.trim().to_ascii_lowercase().as_str() {
        "debug" => LogLevel::Debug,
        "warning" | "warn" => LogLevel::Warning,
        "error" => LogLevel::Error,
        _ => LogLevel::Info,
    }
}

fn now_unix_secs() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    i64::try_from(secs).unwrap_or(i64::MAX)
}

fn send_reply<T>(
    action: &'static str,
    reply: Option<oneshot::Sender<Result<T, CoreError>>>,
    res: Result<T, CoreError>,
) {
    match reply {
        Some(tx) => {
            let _ = tx.send(res);
        }
        None => {
            if let Err(err) = res {
                tracing::warn!(action, error = %err, "fire-and-forget coordinator command failed");
            }
        }
    }
}
