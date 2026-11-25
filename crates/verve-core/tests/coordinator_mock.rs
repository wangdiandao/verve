//! Deterministic headless integration tests using an in-memory mock `ProxyKernel`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::{Notify, mpsc};
use tokio_stream_wrappers::UnboundedReceiverStream;
use verve_compiler::CompilerPipeline;
use verve_config::ConfigManager;
use verve_core::{
    AppCoordinator, CoordinatorConfig, CoreError, DomainDirtyEvent, DomainKind, SnapshotStore,
    TelemetryHub,
};
use verve_kernel::{
    ConfigDiagnostic, ControlEndpoint, DirectProcessSupervisor, KernelCapabilities, KernelError,
    KernelSupervisor, LogEntry, LogLevel, ProviderKind, ProxyGroup, ProxyGroupKind, ProxyKernel,
    ProxyMode, ProxyNode, ProxyTopology, RuleEntry, RuleProviderInfo, RuleSnapshot, TelemetryEvent,
    TelemetryKind, TelemetryStream, TrafficSample,
};

mod tokio_stream_wrappers {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use futures_util::Stream;
    use tokio::sync::mpsc;

    pub struct UnboundedReceiverStream<T>(pub mpsc::UnboundedReceiver<T>);

    impl<T> Stream for UnboundedReceiverStream<T> {
        type Item = T;

        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            self.0.poll_recv(cx)
        }
    }
}

struct MockState {
    preflight_reject_needle: Option<String>,
    topology: ProxyTopology,
    rules: RuleSnapshot,
    delays: BTreeMap<String, u16>,
    open_telemetry_calls: AtomicUsize,
    stream_dropped: AtomicBool,
    stream_drop_notify: Notify,
    traffic_script: Mutex<Vec<TrafficSample>>,
    log_script: Mutex<Vec<LogEntry>>,
    keep_stream_open: AtomicBool,
}

impl Default for MockState {
    fn default() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            "hk-01".into(),
            ProxyNode {
                name: "hk-01".into(),
                protocol: "ss".into(),
                udp: true,
                latest_delay_ms: None,
                alive: true,
            },
        );
        nodes.insert(
            "jp-01".into(),
            ProxyNode {
                name: "jp-01".into(),
                protocol: "ss".into(),
                udp: true,
                latest_delay_ms: None,
                alive: true,
            },
        );

        let topology = ProxyTopology {
            groups: vec![ProxyGroup {
                name: "Proxy".into(),
                kind: ProxyGroupKind::Select,
                now: Some("hk-01".into()),
                all: vec!["hk-01".into(), "jp-01".into(), "DIRECT".into()],
                hidden: false,
            }],
            nodes,
        };

        let rules = RuleSnapshot {
            rules: vec![RuleEntry {
                kind: "MATCH".into(),
                payload: String::new(),
                proxy: "Proxy".into(),
            }],
            providers: vec![RuleProviderInfo {
                name: "reject".into(),
                behavior: "domain".into(),
                vehicle_type: "HTTP".into(),
                rule_count: 42,
                updated_at: None,
            }],
        };

        let mut delays = BTreeMap::new();
        delays.insert("hk-01".into(), 33);
        delays.insert("jp-01".into(), 67);

        Self {
            preflight_reject_needle: Some("invalid-rule-target".into()),
            topology,
            rules,
            delays,
            open_telemetry_calls: AtomicUsize::new(0),
            stream_dropped: AtomicBool::new(false),
            stream_drop_notify: Notify::new(),
            traffic_script: Mutex::new(Vec::new()),
            log_script: Mutex::new(Vec::new()),
            keep_stream_open: AtomicBool::new(false),
        }
    }
}

#[derive(Clone, Default)]
struct MockKernel {
    state: Arc<Mutex<MockState>>,
}

struct DropSignalStream<S> {
    inner: S,
    state: Arc<Mutex<MockState>>,
}

impl<S: futures_util::Stream + Unpin> futures_util::Stream for DropSignalStream<S> {
    type Item = S::Item;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.inner).poll_next(cx)
    }
}

impl<S> Drop for DropSignalStream<S> {
    fn drop(&mut self) {
        let guard = self.state.lock().expect("lock mock state");
        guard.stream_dropped.store(true, Ordering::SeqCst);
        guard.stream_drop_notify.notify_waiters();
    }
}

impl ProxyKernel for MockKernel {
    fn capabilities(&self) -> KernelCapabilities {
        KernelCapabilities {
            supports_tun: true,
            supports_traffic_stream: true,
            supports_memory_stream: true,
            supports_connections_stream: true,
            supports_logs_stream: true,
            supports_delay_test: true,
            supports_providers: true,
        }
    }

    async fn validate_config(&self, path: &Path) -> Result<(), Vec<ConfigDiagnostic>> {
        let content = std::fs::read_to_string(path).map_err(|err| {
            vec![ConfigDiagnostic {
                line: None,
                column: None,
                message: err.to_string(),
            }]
        })?;
        let reject = self
            .state
            .lock()
            .expect("lock mock state")
            .preflight_reject_needle
            .clone();
        if let Some(needle) = reject
            && content.contains(&needle)
        {
            return Err(vec![ConfigDiagnostic {
                line: Some(7),
                column: Some(3),
                message: format!("mock preflight rejected `{needle}`"),
            }]);
        }
        Ok(())
    }

    async fn reload_config(&self, _path: &Path) -> Result<(), KernelError> {
        Ok(())
    }

    async fn fetch_proxies(&self) -> Result<ProxyTopology, KernelError> {
        Ok(self.state.lock().expect("lock mock state").topology.clone())
    }

    async fn select_proxy(&self, group: &str, name: &str) -> Result<(), KernelError> {
        let mut guard = self.state.lock().expect("lock mock state");
        let target = guard
            .topology
            .groups
            .iter_mut()
            .find(|g| g.name == group)
            .ok_or_else(|| KernelError::Protocol(format!("unknown group `{group}`")))?;
        if !target.all.iter().any(|item| item == name) {
            return Err(KernelError::Protocol(format!(
                "node `{name}` not found in group `{group}`"
            )));
        }
        target.now = Some(name.to_owned());
        Ok(())
    }

    async fn test_delay(
        &self,
        name: &str,
        _url: &str,
        _timeout: Duration,
    ) -> Result<u16, KernelError> {
        let guard = self.state.lock().expect("lock mock state");
        guard
            .delays
            .get(name)
            .copied()
            .ok_or_else(|| KernelError::Protocol(format!("unknown node `{name}`")))
    }

    async fn fetch_rules(&self) -> Result<RuleSnapshot, KernelError> {
        Ok(self.state.lock().expect("lock mock state").rules.clone())
    }

    async fn update_provider(&self, _kind: ProviderKind, _name: &str) -> Result<(), KernelError> {
        Ok(())
    }

    async fn close_connection(&self, _id: Option<&str>) -> Result<(), KernelError> {
        Ok(())
    }

    async fn open_telemetry(&self, kind: TelemetryKind) -> Result<TelemetryStream, KernelError> {
        let (tx, rx) = mpsc::unbounded_channel();
        let keep_open = {
            let guard = self.state.lock().expect("lock mock state");
            guard.open_telemetry_calls.fetch_add(1, Ordering::SeqCst);
            guard.stream_dropped.store(false, Ordering::SeqCst);

            match kind {
                TelemetryKind::Traffic => {
                    for sample in guard.traffic_script.lock().unwrap().drain(..) {
                        let _ = tx.send(Ok(TelemetryEvent::Traffic(sample)));
                    }
                }
                TelemetryKind::Logs => {
                    for entry in guard.log_script.lock().unwrap().drain(..) {
                        let _ = tx.send(Ok(TelemetryEvent::Log(entry)));
                    }
                }
                TelemetryKind::Memory => {
                    let _ = tx.send(Ok(TelemetryEvent::Memory(verve_kernel::MemorySample {
                        inuse_bytes: 12_345_678,
                    })));
                }
                TelemetryKind::Connections => {
                    let _ = tx.send(Ok(TelemetryEvent::Connections {
                        upload_total: 100,
                        download_total: 200,
                        connections: Vec::new(),
                    }));
                }
            }
            guard.keep_stream_open.load(Ordering::SeqCst)
        };

        let base = UnboundedReceiverStream(rx);
        if keep_open {
            // Retain sender so stream stays pending until dropped on lease release.
            let kept = base.chain(futures_util::stream::pending());
            let stream = DropSignalStream {
                inner: Box::pin(async_move_holder(tx, kept)),
                state: Arc::clone(&self.state),
            };
            Ok(Box::pin(stream))
        } else {
            drop(tx);
            let stream = DropSignalStream {
                inner: base,
                state: Arc::clone(&self.state),
            };
            Ok(Box::pin(stream))
        }
    }
}

fn async_move_holder<T, S>(
    _keep: mpsc::UnboundedSender<T>,
    stream: S,
) -> impl futures_util::Stream<Item = S::Item>
where
    S: futures_util::Stream + Unpin,
{
    futures_util::stream::unfold((_keep, stream), |(keep, mut s)| async move {
        let next = s.next().await?;
        Some((next, (keep, s)))
    })
}

fn minimal_yaml(marker: &str) -> String {
    format!(
        "proxies:\n  - name: hk-01\n    type: ss\n    server: 198.51.100.1\n    port: 8388\n    cipher: aes-128-gcm\n    password: pass\nrules:\n  - DOMAIN,{marker}.example,DIRECT\n  - MATCH,DIRECT\n"
    )
}

#[tokio::test]
async fn snapshot_store_preserves_untouched_arc_pointers_and_revisions() {
    let store = SnapshotStore::default();
    let mut dirty_rx = store.subscribe();
    let s0 = store.snapshot();
    assert_eq!(s0.runtime.revision, 0);
    assert_eq!(s0.telemetry.revision, 0);
    assert_eq!(s0.proxies.revision, 0);
    assert_eq!(s0.profiles.revision, 0);
    assert_eq!(s0.activity.revision, 0);
    assert_eq!(s0.settings.revision, 0);

    // 1. Mutate profiles only.
    let rev = store.publish_profiles(|p| p.global_merge_enabled = true);
    assert_eq!(rev, 1);
    let s1 = store.snapshot();
    assert!(!Arc::ptr_eq(&s0.profiles, &s1.profiles));
    assert_eq!(s1.profiles.revision, 1);
    assert!(Arc::ptr_eq(&s0.runtime, &s1.runtime));
    assert!(Arc::ptr_eq(&s0.telemetry, &s1.telemetry));
    assert!(Arc::ptr_eq(&s0.proxies, &s1.proxies));
    assert!(Arc::ptr_eq(&s0.activity, &s1.activity));
    assert!(Arc::ptr_eq(&s0.settings, &s1.settings));
    assert_eq!(
        dirty_rx.recv().await.unwrap(),
        DomainDirtyEvent {
            domain: DomainKind::Profiles,
            revision: 1,
        }
    );

    // 2. Mutate proxies only.
    let rev = store.publish_proxies(|px| {
        px.groups.push(ProxyGroup {
            name: "Proxy".into(),
            kind: ProxyGroupKind::Select,
            now: None,
            all: vec!["DIRECT".into()],
            hidden: false,
        });
    });
    assert_eq!(rev, 1);
    let s2 = store.snapshot();
    assert!(!Arc::ptr_eq(&s1.proxies, &s2.proxies));
    assert_eq!(s2.proxies.revision, 1);
    assert!(Arc::ptr_eq(&s1.runtime, &s2.runtime));
    assert!(Arc::ptr_eq(&s1.telemetry, &s2.telemetry));
    assert!(Arc::ptr_eq(&s1.profiles, &s2.profiles));
    assert!(Arc::ptr_eq(&s1.activity, &s2.activity));
    assert!(Arc::ptr_eq(&s1.settings, &s2.settings));
    assert_eq!(
        dirty_rx.recv().await.unwrap(),
        DomainDirtyEvent {
            domain: DomainKind::Proxies,
            revision: 1,
        }
    );

    // 3. Mutate telemetry only.
    let rev = store.publish_telemetry(|t| {
        t.upload_bps = 1024;
        t.download_bps = 4096;
    });
    assert_eq!(rev, 1);
    let s3 = store.snapshot();
    assert!(!Arc::ptr_eq(&s2.telemetry, &s3.telemetry));
    assert_eq!(s3.telemetry.revision, 1);
    assert!(Arc::ptr_eq(&s2.runtime, &s3.runtime));
    assert!(Arc::ptr_eq(&s2.proxies, &s3.proxies));
    assert!(Arc::ptr_eq(&s2.profiles, &s3.profiles));
    assert!(Arc::ptr_eq(&s2.activity, &s3.activity));
    assert!(Arc::ptr_eq(&s2.settings, &s3.settings));
}

#[tokio::test]
async fn activate_profile_preflight_failure_preserves_active_and_backup_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_manager = ConfigManager::open(dir.path()).expect("config manager");
    let runtime_dir = config_manager.runtime_dir();
    let kernel = Arc::new(MockKernel::default());

    let mut handle = AppCoordinator::spawn(
        config_manager,
        CompilerPipeline::default(),
        KernelSupervisor::Direct(DirectProcessSupervisor::new()),
        Arc::clone(&kernel),
        CoordinatorConfig {
            control_endpoint: ControlEndpoint::default_for_session("mock-preflight"),
            ..Default::default()
        },
    )
    .expect("spawn coordinator");

    // Import and activate v1.
    handle
        .bus()
        .import_local_profile("p1", "Profile One", minimal_yaml("alpha"), None)
        .await
        .expect("import p1");
    handle
        .bus()
        .activate_profile("p1")
        .await
        .expect("activate p1");

    let active_v1 = std::fs::read_to_string(runtime_dir.join("active.yaml")).unwrap();
    assert!(active_v1.contains("alpha.example"));
    assert!(!runtime_dir.join("last_known_good.yaml").exists());

    // Import and activate v2 -> backs up v1 into last_known_good.yaml.
    handle
        .bus()
        .import_local_profile("p2", "Profile Two", minimal_yaml("beta"), None)
        .await
        .expect("import p2");
    handle
        .bus()
        .activate_profile("p2")
        .await
        .expect("activate p2");

    let active_v2 = std::fs::read_to_string(runtime_dir.join("active.yaml")).unwrap();
    let backup_v1 = std::fs::read_to_string(runtime_dir.join("last_known_good.yaml")).unwrap();
    assert!(active_v2.contains("beta.example"));
    assert_eq!(backup_v1, active_v1);

    // Snapshot isolation check during proxy refresh & rules refresh.
    let before_refresh = handle.snapshot();
    handle
        .bus()
        .refresh_proxies()
        .await
        .expect("refresh proxies");
    let after_proxies = handle.snapshot();
    assert!(!Arc::ptr_eq(
        &before_refresh.proxies,
        &after_proxies.proxies
    ));
    assert!(Arc::ptr_eq(
        &before_refresh.profiles,
        &after_proxies.profiles
    ));
    assert!(Arc::ptr_eq(
        &before_refresh.telemetry,
        &after_proxies.telemetry
    ));
    assert!(Arc::ptr_eq(
        &before_refresh.settings,
        &after_proxies.settings
    ));

    // Import a broken profile that fails kernel preflight validation.
    handle
        .bus()
        .import_local_profile(
            "p-bad",
            "Broken Profile",
            minimal_yaml("invalid-rule-target"),
            None,
        )
        .await
        .expect("import p-bad");

    let err = handle
        .bus()
        .activate_profile("p-bad")
        .await
        .expect_err("preflight failure must fail activation");
    match err {
        CoreError::Compile(diag) => {
            assert_eq!(diag.stage, "preflight");
            assert_eq!(diag.line, Some(7));
            assert_eq!(diag.column, Some(3));
            assert!(diag.message.contains("invalid-rule-target"));

            let snap = handle.snapshot();
            assert_eq!(snap.profiles.active_profile_id.as_deref(), Some("p2"));
            assert_eq!(snap.profiles.last_compile_diagnostic.as_ref(), Some(&diag));
        }
        other => panic!("expected CoreError::Compile, got {other:?}"),
    }

    // active.yaml & last_known_good.yaml remain untouched.
    assert_eq!(
        std::fs::read_to_string(runtime_dir.join("active.yaml")).unwrap(),
        active_v2
    );
    assert_eq!(
        std::fs::read_to_string(runtime_dir.join("last_known_good.yaml")).unwrap(),
        active_v1
    );

    // Subsequent successful activation clears last_compile_diagnostic.
    handle
        .bus()
        .activate_profile("p1")
        .await
        .expect("reactivate p1");
    assert!(handle.snapshot().profiles.last_compile_diagnostic.is_none());

    // Verify select_proxy, test_delay, set_proxy_mode, and refresh_rules through coordinator.
    handle
        .bus()
        .select_proxy("Proxy", "jp-01")
        .await
        .expect("select jp-01");
    assert_eq!(
        handle.snapshot().proxies.groups[0].now.as_deref(),
        Some("jp-01")
    );

    let delay = handle
        .bus()
        .test_delay(
            "jp-01",
            "http://www.gstatic.com/generate_204",
            Duration::from_secs(2),
        )
        .await
        .expect("test delay");
    assert_eq!(delay, 67);
    assert_eq!(
        handle.snapshot().proxies.nodes["jp-01"].latest_delay_ms,
        Some(67)
    );

    handle
        .bus()
        .set_proxy_mode(ProxyMode::Global)
        .await
        .expect("set global mode");
    assert_eq!(handle.snapshot().runtime.proxy_mode, ProxyMode::Global);
    assert_eq!(handle.snapshot().settings.syncable.proxy_mode, "global");

    let rules = handle.bus().refresh_rules().await.expect("refresh rules");
    assert_eq!(rules.rules.len(), 1);
    assert_eq!(handle.snapshot().activity.rules.len(), 1);
    assert_eq!(handle.snapshot().activity.rule_providers.len(), 1);

    handle.shutdown().await.expect("clean shutdown");
}

#[tokio::test]
async fn telemetry_hub_ref_counts_cancels_on_last_drop_and_bounds_ring_buffers() {
    let snapshots = Arc::new(SnapshotStore::default());
    let kernel = Arc::new(MockKernel::default());

    {
        let guard = kernel.state.lock().unwrap();
        guard.keep_stream_open.store(true, Ordering::SeqCst);
        *guard.traffic_script.lock().unwrap() = vec![
            TrafficSample {
                up_bps: 10,
                down_bps: 20,
            },
            TrafficSample {
                up_bps: 30,
                down_bps: 40,
            },
            TrafficSample {
                up_bps: 50,
                down_bps: 60,
            },
            TrafficSample {
                up_bps: 70,
                down_bps: 80,
            },
        ];
        *guard.log_script.lock().unwrap() = vec![
            LogEntry {
                level: LogLevel::Info,
                payload: "l1".into(),
                timestamp_unix_ms: 1,
            },
            LogEntry {
                level: LogLevel::Warning,
                payload: "l2".into(),
                timestamp_unix_ms: 2,
            },
            LogEntry {
                level: LogLevel::Error,
                payload: "l3".into(),
                timestamp_unix_ms: 3,
            },
        ];
    }

    let hub = TelemetryHub::with_capacities(Arc::clone(&kernel), Arc::clone(&snapshots), 3, 2);

    // 0 -> 1 opens stream once.
    let lease1 = hub.acquire(TelemetryKind::Traffic);
    assert_eq!(hub.ref_count(TelemetryKind::Traffic), 1);

    // Wait until all 4 traffic samples have arrived into the capacity-3 ring buffer.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if snapshots.snapshot().telemetry.upload_bps == 70 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("traffic samples should populate snapshot");

    assert_eq!(
        kernel
            .state
            .lock()
            .unwrap()
            .open_telemetry_calls
            .load(Ordering::SeqCst),
        1
    );
    let snap = snapshots.snapshot();
    assert_eq!(snap.telemetry.traffic_history.len(), 3);
    let ups: Vec<u64> = snap
        .telemetry
        .traffic_history
        .iter()
        .map(|s| s.up_bps)
        .collect();
    assert_eq!(ups, vec![30, 50, 70]);

    // 1 -> 2 shares the active stream without opening a second stream.
    let lease2 = hub.acquire(TelemetryKind::Traffic);
    assert_eq!(hub.ref_count(TelemetryKind::Traffic), 2);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        kernel
            .state
            .lock()
            .unwrap()
            .open_telemetry_calls
            .load(Ordering::SeqCst),
        1
    );

    // 2 -> 1 keeps stream open.
    drop(lease1);
    assert_eq!(hub.ref_count(TelemetryKind::Traffic), 1);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        !kernel
            .state
            .lock()
            .unwrap()
            .stream_dropped
            .load(Ordering::SeqCst),
        "stream must remain open while lease2 is held"
    );

    // 1 -> 0 cancels token and drops the underlying stream.
    drop(lease2);
    assert_eq!(hub.ref_count(TelemetryKind::Traffic), 0);

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if kernel
                .state
                .lock()
                .unwrap()
                .stream_dropped
                .load(Ordering::SeqCst)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("dropping final lease must cancel and drop the stream");

    // Verify bounded log ring buffer (capacity 2 keeps newest 2 entries).
    let log_lease = hub.acquire(TelemetryKind::Logs);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if snapshots
                .snapshot()
                .activity
                .logs
                .back()
                .is_some_and(|entry| entry.payload == "l3")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("logs should populate ActivitySlice");

    let logs: Vec<String> = snapshots
        .snapshot()
        .activity
        .logs
        .iter()
        .map(|l| l.payload.clone())
        .collect();
    assert_eq!(logs, vec!["l2", "l3"]);

    drop(log_lease);
    hub.shutdown().await;
}
