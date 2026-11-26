//! Headless integration tests for P0 `verve-core` commands:
//! remote subscription import/refresh, profile deletion, kernel restart & version tracking,
//! concurrent batch delay testing, network/TUN settings updates, theme seed color,
//! auto-launch, service mode, and service lifecycle actions.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use verve_compiler::CompilerPipeline;
use verve_config::{ConfigManager, ProfileSource, TunConfig};
use verve_core::{
    AppCoordinator, CoordinatorConfig, CoreError, FetchedSubscription, ServiceControlAction,
    ServiceControlBackend, ServiceStatus, SubscriptionFetcher, SubscriptionUserInfo,
};
use verve_kernel::{
    ConfigDiagnostic, ControlEndpoint, DirectProcessSupervisor, KernelCapabilities, KernelError,
    KernelSupervisor, ProviderKind, ProxyGroup, ProxyGroupKind, ProxyKernel, ProxyNode,
    ProxyTopology, RemoteKernelStatus, RuleSnapshot, ServiceIpcClient, TelemetryKind,
    TelemetryStream,
};
use verve_sys::{AutoLaunchBackend, InMemoryAutoLaunchBackend, InMemorySystemProxyBackend};

#[derive(Default)]
struct MockKernelState {
    version: Option<String>,
    topology: ProxyTopology,
    delays: BTreeMap<String, Result<u16, String>>,
    reload_count: AtomicUsize,
    probed_nodes: Mutex<Vec<String>>,
}

#[derive(Clone)]
struct P0MockKernel {
    state: Arc<MockKernelState>,
}

impl P0MockKernel {
    fn new() -> Self {
        let mut nodes = BTreeMap::new();
        for name in ["hk-01", "jp-01", "sg-01", "us-fail"] {
            nodes.insert(
                name.to_owned(),
                ProxyNode {
                    name: name.to_owned(),
                    protocol: "vless".into(),
                    udp: true,
                    latest_delay_ms: None,
                    alive: true,
                },
            );
        }

        let topology = ProxyTopology {
            groups: vec![
                ProxyGroup {
                    name: "Proxy".into(),
                    kind: ProxyGroupKind::Select,
                    now: Some("hk-01".into()),
                    all: vec![
                        "Auto".into(),
                        "hk-01".into(),
                        "jp-01".into(),
                        "DIRECT".into(),
                        "REJECT".into(),
                        "PASS".into(),
                        "COMPATIBLE".into(),
                    ],
                    hidden: false,
                },
                ProxyGroup {
                    name: "Auto".into(),
                    kind: ProxyGroupKind::UrlTest,
                    now: Some("sg-01".into()),
                    all: vec!["sg-01".into(), "us-fail".into(), "hk-01".into()],
                    hidden: false,
                },
                ProxyGroup {
                    name: "HiddenGroup".into(),
                    kind: ProxyGroupKind::Select,
                    now: Some("hk-01".into()),
                    all: vec!["hk-01".into()],
                    hidden: true,
                },
            ],
            nodes,
        };

        let mut delays = BTreeMap::new();
        delays.insert("hk-01".into(), Ok(35));
        delays.insert("jp-01".into(), Ok(72));
        delays.insert("sg-01".into(), Ok(48));
        delays.insert("us-fail".into(), Err("timeout".into()));

        Self {
            state: Arc::new(MockKernelState {
                version: Some("Mihomo Meta v1.19.0".into()),
                topology,
                delays,
                reload_count: AtomicUsize::new(0),
                probed_nodes: Mutex::new(Vec::new()),
            }),
        }
    }

    fn reload_count(&self) -> usize {
        self.state.reload_count.load(Ordering::SeqCst)
    }

    fn probed_nodes(&self) -> Vec<String> {
        self.state.probed_nodes.lock().unwrap().clone()
    }
}

impl ProxyKernel for P0MockKernel {
    fn capabilities(&self) -> KernelCapabilities {
        KernelCapabilities::default()
    }

    async fn fetch_version(&self) -> Result<Option<String>, KernelError> {
        Ok(self.state.version.clone())
    }

    async fn validate_config(&self, _path: &Path) -> Result<(), Vec<ConfigDiagnostic>> {
        Ok(())
    }

    async fn reload_config(&self, _path: &Path) -> Result<(), KernelError> {
        self.state.reload_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn fetch_proxies(&self) -> Result<ProxyTopology, KernelError> {
        Ok(self.state.topology.clone())
    }

    async fn select_proxy(&self, _group: &str, _name: &str) -> Result<(), KernelError> {
        Ok(())
    }

    async fn test_delay(
        &self,
        name: &str,
        _url: &str,
        _timeout: Duration,
    ) -> Result<u16, KernelError> {
        self.state
            .probed_nodes
            .lock()
            .unwrap()
            .push(name.to_owned());
        match self.state.delays.get(name) {
            Some(Ok(ms)) => Ok(*ms),
            Some(Err(msg)) => Err(KernelError::Protocol(msg.clone())),
            None => Err(KernelError::Protocol(format!("unexpected probe `{name}`"))),
        }
    }

    async fn fetch_rules(&self) -> Result<RuleSnapshot, KernelError> {
        Ok(RuleSnapshot::default())
    }

    async fn update_provider(&self, _kind: ProviderKind, _name: &str) -> Result<(), KernelError> {
        Ok(())
    }

    async fn close_connection(&self, _id: Option<&str>) -> Result<(), KernelError> {
        Ok(())
    }

    async fn open_telemetry(&self, _kind: TelemetryKind) -> Result<TelemetryStream, KernelError> {
        Ok(Box::pin(futures_util::stream::pending()))
    }
}

type FetchResponse = Result<FetchedSubscription, String>;

struct ScriptedSubscriptionFetcher {
    next: Mutex<Vec<FetchResponse>>,
}

impl ScriptedSubscriptionFetcher {
    fn new(responses: Vec<FetchResponse>) -> Self {
        let mut rev = responses;
        rev.reverse();
        Self {
            next: Mutex::new(rev),
        }
    }
}

impl SubscriptionFetcher for ScriptedSubscriptionFetcher {
    fn fetch<'a>(
        &'a self,
        _url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<FetchedSubscription, CoreError>> + Send + 'a>> {
        Box::pin(async move {
            let item =
                self.next.lock().unwrap().pop().ok_or_else(|| {
                    CoreError::Subscription("no scripted fetch response left".into())
                })?;
            item.map_err(CoreError::Subscription)
        })
    }
}

struct MockServiceIpc {
    available: AtomicBool,
    running: AtomicBool,
    pid: AtomicUsize,
}

impl MockServiceIpc {
    fn new(available: bool) -> Self {
        Self {
            available: AtomicBool::new(available),
            running: AtomicBool::new(false),
            pid: AtomicUsize::new(7000),
        }
    }
}

impl ServiceIpcClient for MockServiceIpc {
    fn start_remote<'a>(
        &'a self,
        _kernel_id: &'a str,
        _bin_path: &'a Path,
        _work_dir: &'a Path,
        _config_path: &'a Path,
        _ctrl_endpoint: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<u32, KernelError>> + Send + 'a>> {
        Box::pin(async move {
            if !self.available.load(Ordering::SeqCst) {
                return Err(KernelError::Protocol("service unavailable".into()));
            }
            self.running.store(true, Ordering::SeqCst);
            let next_pid = self.pid.fetch_add(1, Ordering::SeqCst) as u32;
            Ok(next_pid)
        })
    }

    fn stop_remote<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<(), KernelError>> + Send + 'a>> {
        Box::pin(async move {
            self.running.store(false, Ordering::SeqCst);
            Ok(())
        })
    }

    fn query_remote_status<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<RemoteKernelStatus, KernelError>> + Send + 'a>> {
        Box::pin(async move {
            if !self.available.load(Ordering::SeqCst) {
                return Err(KernelError::Protocol("pipe not found".into()));
            }
            let running = self.running.load(Ordering::SeqCst);
            Ok(RemoteKernelStatus {
                running,
                pid: running.then_some(self.pid.load(Ordering::SeqCst) as u32),
                ctrl_endpoint: None,
            })
        })
    }

    fn read_remote_stderr_tail<'a>(
        &'a self,
        _max_lines: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, KernelError>> + Send + 'a>> {
        Box::pin(async move { Ok(Vec::new()) })
    }
}

struct MockServiceControl {
    ipc: Arc<MockServiceIpc>,
    installed_dirs: Mutex<Vec<PathBuf>>,
    uninstall_calls: AtomicUsize,
}

impl ServiceControlBackend for MockServiceControl {
    fn install_service<'a>(
        &'a self,
        trusted_dirs: &'a [PathBuf],
    ) -> Pin<Box<dyn Future<Output = Result<(), CoreError>> + Send + 'a>> {
        let dirs = trusted_dirs.to_vec();
        Box::pin(async move {
            *self.installed_dirs.lock().unwrap() = dirs;
            self.ipc.available.store(true, Ordering::SeqCst);
            Ok(())
        })
    }

    fn uninstall_service<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<(), CoreError>> + Send + 'a>> {
        Box::pin(async move {
            self.uninstall_calls.fetch_add(1, Ordering::SeqCst);
            self.ipc.available.store(false, Ordering::SeqCst);
            self.ipc.running.store(false, Ordering::SeqCst);
            Ok(())
        })
    }
}

#[tokio::test]
async fn remote_profile_import_refresh_rollback_and_delete_lifecycle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg_mgr = ConfigManager::open(dir.path()).expect("config manager");
    let profiles_dir = cfg_mgr.profiles_dir();
    let runtime_dir = cfg_mgr.runtime_dir();

    let fetcher = Arc::new(ScriptedSubscriptionFetcher::new(vec![
        // 1. Initial import
        Ok(FetchedSubscription {
            raw_yaml: "mixed-port: 7890\nrules:\n  - DOMAIN,v1.example,DIRECT\n".into(),
            suggested_name: Some("Work Sub".into()),
            usage: Some(SubscriptionUserInfo {
                upload_bytes: 100,
                download_bytes: 200,
                total_bytes: Some(1_000),
                expire_unix_secs: Some(1_900_000_000),
            }),
        }),
        // 2. Refresh fails with network error -> existing YAML must remain intact
        Err("connection reset".into()),
        // 3. Refresh returns invalid YAML -> existing YAML must remain intact
        Ok(FetchedSubscription {
            raw_yaml: "- not a mapping".into(),
            suggested_name: None,
            usage: None,
        }),
        // 4. Refresh succeeds -> updates YAML and usage, reloads active kernel
        Ok(FetchedSubscription {
            raw_yaml: "mixed-port: 7890\nrules:\n  - DOMAIN,v2.example,DIRECT\n".into(),
            suggested_name: Some("Work Sub".into()),
            usage: Some(SubscriptionUserInfo {
                upload_bytes: 300,
                download_bytes: 400,
                total_bytes: Some(2_000),
                expire_unix_secs: Some(1_950_000_000),
            }),
        }),
    ]));

    let kernel = Arc::new(P0MockKernel::new());
    let service_ipc = Arc::new(MockServiceIpc::new(true));
    let fake_bin = dir.path().join("fake-mihomo");
    std::fs::write(&fake_bin, b"stub").unwrap();

    let mut handle = AppCoordinator::spawn(
        cfg_mgr,
        CompilerPipeline::default(),
        KernelSupervisor::Direct(DirectProcessSupervisor::new()),
        Arc::clone(&kernel),
        CoordinatorConfig {
            control_endpoint: ControlEndpoint::default_for_session("p0-remote-profile"),
            kernel_binary_path: Some(fake_bin),
            system_proxy_backend: Some(Arc::new(InMemorySystemProxyBackend::default())),
            service_ipc_client: Some(service_ipc),
            subscription_fetcher: Some(fetcher),
            ..Default::default()
        },
    )
    .expect("spawn coordinator");

    // 1. Import remote profile
    let entry = handle
        .bus()
        .import_remote_profile("https://sub.example.com/profile.yaml", None)
        .await
        .expect("remote import must succeed");
    assert_eq!(entry.id, "work-sub");
    assert_eq!(entry.name, "Work Sub");
    assert!(matches!(entry.source, ProfileSource::Remote { .. }));
    assert_eq!(
        std::fs::read_to_string(profiles_dir.join(&entry.file_name)).unwrap(),
        "mixed-port: 7890\nrules:\n  - DOMAIN,v1.example,DIRECT\n"
    );
    let usage = handle
        .snapshot()
        .profiles
        .usages
        .get(&entry.id)
        .cloned()
        .expect("usage metadata must be published");
    assert_eq!(usage.download_bytes, 200);
    assert_eq!(usage.total_bytes, Some(1_000));

    // Activate and start under mock service mode so supervisor is running.
    handle.bus().set_service_mode(true).await.unwrap();
    handle.bus().activate_profile(&entry.id).await.unwrap();
    let pid = handle.bus().start_kernel().await.unwrap();
    assert!(handle.snapshot().runtime.kernel_running);
    assert_eq!(handle.snapshot().runtime.kernel_pid, Some(pid));
    assert_eq!(
        handle.snapshot().runtime.kernel_version.as_deref(),
        Some("Mihomo Meta v1.19.0")
    );
    assert!(
        handle
            .snapshot()
            .runtime
            .kernel_started_at_unix_secs
            .is_some()
    );

    // 2. Refresh network error must preserve existing YAML on disk.
    let err = handle
        .bus()
        .refresh_profile(&entry.id)
        .await
        .expect_err("network failure must return error");
    assert!(matches!(err, CoreError::Subscription(_)));
    assert!(
        std::fs::read_to_string(profiles_dir.join(&entry.file_name))
            .unwrap()
            .contains("v1.example")
    );

    // 3. Refresh invalid YAML must preserve existing YAML on disk.
    let yaml_err = handle
        .bus()
        .refresh_profile(&entry.id)
        .await
        .expect_err("non-mapping YAML must return error");
    assert!(matches!(yaml_err, CoreError::Subscription(_)));
    assert!(
        std::fs::read_to_string(profiles_dir.join(&entry.file_name))
            .unwrap()
            .contains("v1.example")
    );

    // 4. Refresh success updates disk YAML, SQLite usage, and hot-reloads active running kernel.
    let reloads_before = kernel.reload_count();
    handle.bus().refresh_profile(&entry.id).await.unwrap();
    assert!(
        std::fs::read_to_string(profiles_dir.join(&entry.file_name))
            .unwrap()
            .contains("v2.example")
    );
    assert_eq!(kernel.reload_count(), reloads_before + 1);
    assert_eq!(
        handle.snapshot().profiles.usages[&entry.id].download_bytes,
        400
    );

    // 5. Local profile cannot be refreshed, and inactive deletion preserves active kernel.
    let local_entry = handle
        .bus()
        .import_local_profile("local-1", "Local", "mixed-port: 7890\n", None)
        .await
        .unwrap();
    assert!(matches!(
        handle.bus().refresh_profile(&local_entry.id).await,
        Err(CoreError::InvalidInput(_))
    ));
    handle.bus().delete_profile(&local_entry.id).await.unwrap();
    assert!(!profiles_dir.join(&local_entry.file_name).exists());
    assert!(handle.snapshot().runtime.kernel_running);

    // 6. Deleting the active profile stops the kernel, clears active_profile_id, and removes files.
    handle.bus().delete_profile(&entry.id).await.unwrap();
    assert!(!handle.snapshot().runtime.kernel_running);
    assert_eq!(handle.snapshot().runtime.kernel_version, None);
    assert_eq!(handle.snapshot().runtime.kernel_started_at_unix_secs, None);
    assert_eq!(handle.snapshot().runtime.active_profile_id, None);
    assert_eq!(handle.snapshot().profiles.active_profile_id, None);
    assert!(!handle.snapshot().profiles.usages.contains_key(&entry.id));
    assert!(!profiles_dir.join(&entry.file_name).exists());
    assert!(!runtime_dir.join("active.yaml").exists());

    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_group_and_all_delay_tests_skip_builtins_and_groups() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg_mgr = ConfigManager::open(dir.path()).expect("config manager");
    let kernel = Arc::new(P0MockKernel::new());

    let mut handle = AppCoordinator::spawn(
        cfg_mgr,
        CompilerPipeline::default(),
        KernelSupervisor::Direct(DirectProcessSupervisor::new()),
        Arc::clone(&kernel),
        CoordinatorConfig {
            control_endpoint: ControlEndpoint::default_for_session("p0-delay-tests"),
            system_proxy_backend: Some(Arc::new(InMemorySystemProxyBackend::default())),
            ..Default::default()
        },
    )
    .expect("spawn coordinator");

    // 1. TestGroupDelay on "Proxy" must test only leaf nodes ("hk-01", "jp-01"), skipping "Auto" & built-ins.
    let group_results = handle
        .bus()
        .test_group_delay(
            "Proxy",
            "http://www.gstatic.com/generate_204",
            Duration::from_secs(3),
        )
        .await
        .expect("group delay test must succeed");
    assert_eq!(group_results.len(), 2);
    assert_eq!(group_results.get("hk-01"), Some(&Some(35)));
    assert_eq!(group_results.get("jp-01"), Some(&Some(72)));
    assert_eq!(
        handle.snapshot().proxies.nodes["hk-01"].latest_delay_ms,
        Some(35)
    );

    // 2. TestAllGroupsDelay tests unique leaf nodes across all visible groups ("hk-01", "jp-01", "sg-01", "us-fail").
    let all_results = handle
        .bus()
        .test_all_groups_delay(
            "http://www.gstatic.com/generate_204",
            Duration::from_secs(3),
        )
        .await
        .expect("all groups delay test must succeed");
    assert_eq!(all_results.len(), 4);
    assert_eq!(all_results.get("hk-01"), Some(&Some(35)));
    assert_eq!(all_results.get("jp-01"), Some(&Some(72)));
    assert_eq!(all_results.get("sg-01"), Some(&Some(48)));
    assert_eq!(all_results.get("us-fail"), Some(&None));

    let probed = kernel.probed_nodes();
    assert!(
        !probed
            .iter()
            .any(|n| n == "DIRECT" || n == "REJECT" || n == "Auto")
    );

    let snap = handle.snapshot();
    assert_eq!(snap.proxies.nodes["sg-01"].latest_delay_ms, Some(48));
    assert_eq!(snap.proxies.nodes["us-fail"].latest_delay_ms, None);

    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn network_tun_seed_autolaunch_restart_and_service_actions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg_mgr = ConfigManager::open(dir.path()).expect("config manager");
    let runtime_dir = cfg_mgr.runtime_dir();

    let kernel = Arc::new(P0MockKernel::new());
    let sysproxy_backend = InMemorySystemProxyBackend::default();
    let autolaunch_backend = InMemoryAutoLaunchBackend::new(false);
    let service_ipc = Arc::new(MockServiceIpc::new(false));
    let service_ctrl = Arc::new(MockServiceControl {
        ipc: Arc::clone(&service_ipc),
        installed_dirs: Mutex::new(Vec::new()),
        uninstall_calls: AtomicUsize::new(0),
    });

    let fake_bin = dir.path().join("fake-mihomo");
    std::fs::write(&fake_bin, b"stub").unwrap();

    let mut handle = AppCoordinator::spawn(
        cfg_mgr,
        CompilerPipeline::default(),
        KernelSupervisor::Direct(DirectProcessSupervisor::new()),
        Arc::clone(&kernel),
        CoordinatorConfig {
            control_endpoint: ControlEndpoint::default_for_session("p0-settings"),
            kernel_binary_path: Some(fake_bin),
            system_proxy_backend: Some(Arc::new(sysproxy_backend.clone())),
            service_ipc_client: Some(Arc::clone(&service_ipc) as Arc<dyn ServiceIpcClient>),
            auto_launch_backend: Some(Arc::new(autolaunch_backend.clone())),
            service_control_backend: Some(
                Arc::clone(&service_ctrl) as Arc<dyn ServiceControlBackend>
            ),
            ..Default::default()
        },
    )
    .expect("spawn coordinator");

    // 1. ServiceAction: CheckStatus -> Unavailable -> Install -> Available -> Uninstall -> Unavailable.
    let status0 = handle
        .bus()
        .service_action(ServiceControlAction::CheckStatus)
        .await
        .unwrap();
    assert!(matches!(status0, ServiceStatus::Unavailable { .. }));

    let status1 = handle
        .bus()
        .service_action(ServiceControlAction::Install)
        .await
        .unwrap();
    assert_eq!(status1, ServiceStatus::Available { running_pid: None });
    assert_eq!(handle.snapshot().runtime.service_status, status1);
    assert!(!service_ctrl.installed_dirs.lock().unwrap().is_empty());

    // 2. Import, activate, enable service mode, start, and restart kernel.
    handle
        .bus()
        .import_local_profile(
            "main",
            "Main",
            "mixed-port: 7890\nrules:\n  - MATCH,DIRECT\n",
            None,
        )
        .await
        .unwrap();
    handle.bus().activate_profile("main").await.unwrap();
    handle.bus().set_service_mode(true).await.unwrap();
    assert!(handle.snapshot().settings.local.service_mode_enabled);

    let pid1 = handle.bus().start_kernel().await.unwrap();
    let pid2 = handle.bus().restart_kernel().await.unwrap();
    assert_ne!(pid1, pid2);
    assert_eq!(handle.snapshot().runtime.kernel_pid, Some(pid2));
    assert_eq!(
        handle.snapshot().runtime.kernel_version.as_deref(),
        Some("Mihomo Meta v1.19.0")
    );

    // 3. UpdateNetworkSettings updates port, re-applies system proxy, and reloads running kernel.
    handle.bus().set_system_proxy(true).await.unwrap();
    let reloads_before = kernel.reload_count();
    handle
        .bus()
        .update_network_settings(
            18899,
            true,
            true,
            vec![" localhost ".into(), "127.0.0.1".into(), "".into()],
        )
        .await
        .unwrap();
    assert_eq!(handle.snapshot().runtime.mixed_port, 18899);
    assert!(handle.snapshot().settings.syncable.allow_lan);
    assert!(handle.snapshot().settings.syncable.ipv6);
    assert_eq!(
        handle.snapshot().settings.syncable.system_proxy_bypass,
        vec!["localhost", "127.0.0.1"]
    );
    assert_eq!(sysproxy_backend.snapshot().port, 18899);
    assert_eq!(kernel.reload_count(), reloads_before + 1);

    // Reject port 0
    assert!(matches!(
        handle
            .bus()
            .update_network_settings(0, false, false, Vec::new())
            .await,
        Err(CoreError::Config(_))
    ));

    // 4. Enable TUN and UpdateTunSettings -> injects full tun block and reloads running kernel.
    handle.bus().set_tun_enabled(true).await.unwrap();
    let tun_reloads_before = kernel.reload_count();
    handle
        .bus()
        .update_tun_settings(
            TunConfig {
                stack: "gvisor".into(),
                auto_route: true,
                strict_route: false,
                dns_hijack: vec!["any:53".into()],
                mtu: 1500,
                route_exclude_address: vec!["10.0.0.0/8".into()],
            },
            Some("verve-tun0".into()),
        )
        .await
        .unwrap();
    assert_eq!(kernel.reload_count(), tun_reloads_before + 1);
    assert_eq!(
        handle
            .snapshot()
            .settings
            .local
            .tun_interface_name
            .as_deref(),
        Some("verve-tun0")
    );
    let active_yaml = std::fs::read_to_string(runtime_dir.join("active.yaml")).unwrap();
    assert!(active_yaml.contains("stack: gvisor"));
    assert!(active_yaml.contains("mtu: 1500"));
    assert!(active_yaml.contains("device: verve-tun0"));
    assert!(active_yaml.contains("10.0.0.0/8"));

    // 5. Theme seed color & auto-launch
    handle
        .bus()
        .set_theme_seed_color(Some(0xFF1A_6585))
        .await
        .unwrap();
    assert_eq!(
        handle.snapshot().settings.syncable.theme_seed_color,
        Some(0xFF1A_6585)
    );
    handle.bus().set_theme_seed_color(None).await.unwrap();
    assert_eq!(handle.snapshot().settings.syncable.theme_seed_color, None);

    handle.bus().set_auto_launch(true).await.unwrap();
    assert!(autolaunch_backend.is_enabled().unwrap());
    assert!(handle.snapshot().settings.local.auto_launch);

    // 6. Uninstall service
    handle.bus().stop_kernel().await.unwrap();
    let status2 = handle
        .bus()
        .service_action(ServiceControlAction::Uninstall)
        .await
        .unwrap();
    assert!(matches!(status2, ServiceStatus::Unavailable { .. }));
    assert_eq!(service_ctrl.uninstall_calls.load(Ordering::SeqCst), 1);

    handle.shutdown().await.unwrap();
}
