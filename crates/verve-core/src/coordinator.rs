//! Single-writer CQRS [`AppCoordinator`] and [`MihomoCoordinatorBuilder`].

use std::collections::BTreeMap;
use std::fmt;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use verve_compiler::{
    CommittedConfig, CompileContext, CompilerPipeline, NoopScriptEngine, RuntimeInjectionConfig,
    ScriptEngine,
};
use verve_config::{
    ConfigError, ConfigManager, ProfileEntry, ProfileSource, StateStore, SyncableConfig,
};
use verve_kernel::{
    ControlEndpoint, DaemonIpcSupervisor, DirectProcessSupervisor, KernelError, KernelSupervisor,
    LogLevel, ProxyKernel, ProxyMode, ProxyTopology, RuleSnapshot, ServiceIpcClient,
};
use verve_kernel_mihomo::{MihomoKernel, MihomoKernelConfig, discover_mihomo_binary};
use verve_sys::{
    InMemorySystemProxyBackend, OsSystemProxyBackend, SysError, SystemProxyBackend,
    SystemProxyManager, SystemProxySnapshot,
};

use crate::commands::{AppCommand, CommandBus};
use crate::error::CoreError;
use crate::snapshot::{AppSnapshot, ProfilesSlice, RuntimeSlice, SettingsSlice, SnapshotStore};
use crate::telemetry::{DEFAULT_LOG_CAPACITY, DEFAULT_TRAFFIC_HISTORY_CAPACITY, TelemetryHub};

const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_COMMAND_BUFFER_CAPACITY: usize = 64;

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

fn default_system_proxy_backend() -> Arc<dyn SystemProxyBackend> {
    let dry_run = std::env::var_os("VERVE_DRY_RUN_SYSPROXY")
        .is_some_and(|val| val == "1" || val.eq_ignore_ascii_case("true"));
    if dry_run {
        Arc::new(InMemorySystemProxyBackend::default())
    } else {
        Arc::new(OsSystemProxyBackend)
    }
}

/// Single-writer CQRS orchestrator serializing configuration, compilation, and kernel control.
pub struct AppCoordinator<K: ProxyKernel> {
    config_manager: ConfigManager,
    state_store: StateStore,
    syncable: SyncableConfig,
    compiler: CompilerPipeline,
    supervisor: KernelSupervisor,
    kernel: Arc<K>,
    snapshots: Arc<SnapshotStore>,
    telemetry: TelemetryHub<K>,
    sysproxy_manager: SystemProxyManager<SharedSystemProxyBackend>,
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
                active_profile_id: syncable.active_profile_id.clone(),
                active_profile_name,
                mixed_port: syncable.mixed_port,
                system_proxy_enabled: syncable.system_proxy_enabled,
                tun_enabled: syncable.tun_enabled,
                control_endpoint: Some(config.control_endpoint.clone()),
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
                local,
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
            compiler,
            supervisor,
            kernel: Arc::clone(&kernel),
            snapshots: Arc::clone(&snapshots),
            telemetry: telemetry.clone(),
            sysproxy_manager,
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
                AppCommand::SetThemeMode { mode, reply } => {
                    let res = self.handle_set_theme_mode(mode);
                    send_reply("set_theme_mode", reply, res);
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
                runtime.last_error = Some(err.to_string());
            });
            return Err(err);
        }

        if let Some(active_id) = self.syncable.active_profile_id.clone() {
            self.apply_saved_selections(&active_id).await;
        }
        self.refresh_proxies_internal().await?;

        self.snapshots.publish_runtime(|runtime| {
            runtime.kernel_running = true;
            runtime.kernel_pid = Some(pid);
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
        });
        Ok(())
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
