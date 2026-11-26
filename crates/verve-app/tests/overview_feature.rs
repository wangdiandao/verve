//! Headless `#[gpui::test]` integration suite for the 0.0.3 Overview dashboard feature slice.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{
    AppContext, Bounds, Entity, TestAppContext, WeakEntity, WindowBounds, WindowOptions, px, size,
};
use gpui_material::MaterialRoot;
use gpui_material::window_chrome::title_bar_window_options_with_height;
use verve_app::features::overview::{
    ProfileExpiryStatus, TerminalShell, compute_quota_summary, format_kernel_uptime,
    format_terminal_proxy_command, resolve_profile_expiry, select_primary_proxy_group,
    uses_two_column_layout,
};
use verve_app::{AppShell, CommandDispatcher, RouteId, TelemetrySource, UiStore};
use verve_core::compiler::CompilerPipeline;
use verve_core::config::{ConfigManager, ProfileEntry, ProfileSource, ProfileUsage};
use verve_core::kernel::{
    ConfigDiagnostic, ControlEndpoint, DirectProcessSupervisor, KernelCapabilities, KernelError,
    KernelSupervisor, ProviderKind, ProxyGroup, ProxyGroupKind, ProxyKernel, ProxyMode, ProxyNode,
    ProxyTopology, RuleEntry, RuleSnapshot, TelemetryEvent, TelemetryKind, TelemetryStream,
    TrafficSample,
};
use verve_core::sys::InMemorySystemProxyBackend;
use verve_core::{AppCoordinator, CoordinatorConfig, ServiceStatus};
use verve_ui_kit::{AppearanceMode, DEFAULT_SEED_ARGB, build_material_theme, init_ui_kit};

static LOCALE_MUTEX: Mutex<()> = Mutex::new(());

#[derive(Clone, Default)]
struct OverviewTestKernel;

impl ProxyKernel for OverviewTestKernel {
    fn capabilities(&self) -> KernelCapabilities {
        KernelCapabilities {
            supports_traffic_stream: true,
            supports_memory_stream: true,
            supports_connections_stream: true,
            supports_logs_stream: true,
            ..Default::default()
        }
    }

    async fn fetch_version(&self) -> Result<Option<String>, KernelError> {
        Ok(Some("mihomo v1.19.0".into()))
    }

    async fn validate_config(&self, _path: &Path) -> Result<(), Vec<ConfigDiagnostic>> {
        Ok(())
    }

    async fn reload_config(&self, _path: &Path) -> Result<(), KernelError> {
        Ok(())
    }

    async fn fetch_proxies(&self) -> Result<ProxyTopology, KernelError> {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            "hk-01".into(),
            ProxyNode {
                name: "hk-01".into(),
                protocol: "vless".into(),
                udp: true,
                latest_delay_ms: Some(36),
                alive: true,
            },
        );
        nodes.insert(
            "jp-02".into(),
            ProxyNode {
                name: "jp-02".into(),
                protocol: "trojan".into(),
                udp: true,
                latest_delay_ms: Some(74),
                alive: true,
            },
        );
        Ok(ProxyTopology {
            groups: vec![ProxyGroup {
                name: "Proxy".into(),
                kind: ProxyGroupKind::Select,
                now: Some("hk-01".into()),
                all: vec!["hk-01".into(), "jp-02".into(), "DIRECT".into()],
                hidden: false,
            }],
            nodes,
        })
    }

    async fn select_proxy(&self, _group: &str, _name: &str) -> Result<(), KernelError> {
        Ok(())
    }

    async fn test_delay(
        &self,
        _name: &str,
        _url: &str,
        _timeout: Duration,
    ) -> Result<u16, KernelError> {
        Ok(36)
    }

    async fn fetch_rules(&self) -> Result<RuleSnapshot, KernelError> {
        Ok(RuleSnapshot {
            rules: vec![RuleEntry {
                kind: "MATCH".into(),
                payload: String::new(),
                proxy: "Proxy".into(),
            }],
            providers: Vec::new(),
        })
    }

    async fn update_provider(&self, _kind: ProviderKind, _name: &str) -> Result<(), KernelError> {
        Ok(())
    }

    async fn close_connection(&self, _id: Option<&str>) -> Result<(), KernelError> {
        Ok(())
    }

    async fn open_telemetry(&self, kind: TelemetryKind) -> Result<TelemetryStream, KernelError> {
        if kind == TelemetryKind::Traffic {
            let sample = futures_util::stream::once(async {
                Ok(TelemetryEvent::Traffic(TrafficSample {
                    up_bps: 4_096,
                    down_bps: 32_768,
                }))
            });
            return Ok(Box::pin(futures_util::StreamExt::chain(
                sample,
                futures_util::stream::pending(),
            )));
        }
        Ok(Box::pin(futures_util::stream::pending()))
    }
}

fn sized_window_options(width: f32, height: f32, cx: &gpui::App) -> WindowOptions {
    let base = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(width), px(height)),
            cx,
        ))),
        window_min_size: Some(size(px(760.0), px(520.0))),
        ..Default::default()
    };
    title_bar_window_options_with_height(base, px(40.0))
}

fn wait_until(
    cx: &mut TestAppContext,
    timeout: Duration,
    label: &str,
    mut pred: impl FnMut(&mut TestAppContext) -> bool,
) {
    let deadline = Instant::now() + timeout;
    loop {
        cx.run_until_parked();
        if pred(cx) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for condition: {label}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn seed_rich_overview_snapshot(snapshots: &verve_core::SnapshotStore) {
    snapshots.publish_profiles(|profiles| {
        profiles.profiles = vec![
            ProfileEntry {
                id: "remote-main".into(),
                name: "Global Remote Sub".into(),
                source: ProfileSource::Remote {
                    url: "https://example.com/sub.yaml".into(),
                    update_interval_mins: Some(60),
                },
                file_name: "remote-main.yaml".into(),
                merge_file_name: None,
                script_file_name: None,
            },
            ProfileEntry {
                id: "local-backup".into(),
                name: "Local Backup".into(),
                source: ProfileSource::Local,
                file_name: "local-backup.yaml".into(),
                merge_file_name: None,
                script_file_name: None,
            },
        ];
        profiles.active_profile_id = Some("remote-main".into());
        profiles.usages.insert(
            "remote-main".into(),
            ProfileUsage {
                profile_id: "remote-main".into(),
                upload_bytes: 12 * 1024 * 1024 * 1024,
                download_bytes: 48 * 1024 * 1024 * 1024,
                total_bytes: Some(100 * 1024 * 1024 * 1024),
                expire_unix_secs: Some(1_893_456_000), // 2030-01-01
                updated_at_unix_secs: 1_795_680_000,
            },
        );
    });

    snapshots.publish_proxies(|proxies| {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            "hk-01".into(),
            ProxyNode {
                name: "hk-01".into(),
                protocol: "vless".into(),
                udp: true,
                latest_delay_ms: Some(36),
                alive: true,
            },
        );
        nodes.insert(
            "jp-02".into(),
            ProxyNode {
                name: "jp-02".into(),
                protocol: "trojan".into(),
                udp: true,
                latest_delay_ms: Some(74),
                alive: true,
            },
        );
        nodes.insert(
            "us-03".into(),
            ProxyNode {
                name: "us-03".into(),
                protocol: "ss".into(),
                udp: false,
                latest_delay_ms: Some(210),
                alive: true,
            },
        );
        proxies.groups = vec![
            ProxyGroup {
                name: "HiddenAuto".into(),
                kind: ProxyGroupKind::Select,
                now: Some("hk-01".into()),
                all: vec!["hk-01".into()],
                hidden: true,
            },
            ProxyGroup {
                name: "AutoBest".into(),
                kind: ProxyGroupKind::UrlTest,
                now: Some("hk-01".into()),
                all: vec!["hk-01".into(), "jp-02".into()],
                hidden: false,
            },
            ProxyGroup {
                name: "Proxy".into(),
                kind: ProxyGroupKind::Select,
                now: Some("hk-01".into()),
                all: vec!["hk-01".into(), "jp-02".into(), "us-03".into()],
                hidden: false,
            },
        ];
        proxies.nodes = nodes;
    });

    snapshots.publish_telemetry(|telemetry| {
        telemetry.download_bps = 1_572_864;
        telemetry.upload_bps = 262_144;
        telemetry.total_download_bytes = 524_288_000;
        telemetry.total_upload_bytes = 104_857_600;
        telemetry.memory_bytes = 48 * 1024 * 1024;
        telemetry.traffic_history.push_back(TrafficSample {
            up_bps: 65_536,
            down_bps: 524_288,
        });
        telemetry.traffic_history.push_back(TrafficSample {
            up_bps: 262_144,
            down_bps: 1_572_864,
        });
    });

    snapshots.publish_runtime(|runtime| {
        runtime.proxy_mode = ProxyMode::Rule;
        runtime.kernel_running = true;
        runtime.kernel_pid = Some(4242);
        runtime.kernel_version = Some("mihomo v1.19.0".into());
        runtime.kernel_started_at_unix_secs = Some(1_795_680_000);
        runtime.active_profile_id = Some("remote-main".into());
        runtime.active_profile_name = Some("Global Remote Sub".into());
        runtime.mixed_port = 7897;
        runtime.system_proxy_enabled = true;
        runtime.tun_enabled = true;
        runtime.service_status = ServiceStatus::Available {
            running_pid: Some(4242),
        };
    });

    snapshots.publish_settings(|settings| {
        settings.local.service_mode_enabled = true;
    });
}

#[gpui::test]
fn overview_renders_rich_state_across_locales_and_breakpoints_with_lease_lifecycle(
    cx: &mut TestAppContext,
) {
    let _locale_guard = LOCALE_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    verve_app::i18n::apply_locale("zh-CN");
    cx.background_executor.allow_parking();

    // Verify pure helpers as part of feature integration coverage.
    assert_eq!(
        format_terminal_proxy_command(TerminalShell::PowerShell, "127.0.0.1", 7897),
        "$env:http_proxy=\"http://127.0.0.1:7897\"; $env:https_proxy=\"http://127.0.0.1:7897\"; $env:all_proxy=\"socks5://127.0.0.1:7897\""
    );
    assert_eq!(
        format_terminal_proxy_command(TerminalShell::Bash, "127.0.0.1", 7897),
        "export http_proxy=http://127.0.0.1:7897 https_proxy=http://127.0.0.1:7897 all_proxy=socks5://127.0.0.1:7897"
    );
    assert_eq!(
        format_terminal_proxy_command(TerminalShell::Zsh, "127.0.0.1", 7897),
        "export http_proxy=http://127.0.0.1:7897 https_proxy=http://127.0.0.1:7897 all_proxy=socks5://127.0.0.1:7897"
    );
    assert_eq!(
        format_terminal_proxy_command(TerminalShell::Fish, "127.0.0.1", 7897),
        "set -x http_proxy http://127.0.0.1:7897; set -x https_proxy http://127.0.0.1:7897; set -x all_proxy socks5://127.0.0.1:7897"
    );
    assert_eq!(
        format_terminal_proxy_command(TerminalShell::Cmd, "127.0.0.1", 7897),
        "set http_proxy=http://127.0.0.1:7897 && set https_proxy=http://127.0.0.1:7897 && set all_proxy=socks5://127.0.0.1:7897"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let cfg_mgr = ConfigManager::open(dir.path()).expect("config manager");
    let sysproxy_backend = Arc::new(InMemorySystemProxyBackend::default());
    let kernel = Arc::new(OverviewTestKernel);

    let mut handle = rt.block_on(async {
        AppCoordinator::spawn(
            cfg_mgr,
            CompilerPipeline::default(),
            KernelSupervisor::Direct(DirectProcessSupervisor::new()),
            Arc::clone(&kernel),
            CoordinatorConfig {
                control_endpoint: ControlEndpoint::default_for_session("gpui-test-overview"),
                system_proxy_backend: Some(sysproxy_backend),
                ..Default::default()
            },
        )
        .expect("spawn coordinator")
    });

    let snapshots = Arc::clone(handle.snapshots());
    seed_rich_overview_snapshot(&snapshots);

    // Sanity-check primary group and quota helpers on seeded snapshot.
    let seeded = snapshots.snapshot();
    assert_eq!(
        select_primary_proxy_group(&seeded.proxies.groups).map(|g| g.name.as_str()),
        Some("Proxy")
    );
    let quota = compute_quota_summary(seeded.profiles.usages.get("remote-main").unwrap());
    assert_eq!(quota.percent, Some(60));
    assert_eq!(
        resolve_profile_expiry(Some(1_893_456_000), 1_795_680_000),
        ProfileExpiryStatus::Valid("2030-01-01".into())
    );
    assert_eq!(
        format_kernel_uptime(true, Some(1_795_680_000), 1_795_683_661),
        "01:01:01"
    );

    cx.update(|cx| {
        let light = build_material_theme(AppearanceMode::Light, Some(DEFAULT_SEED_ARGB), false);
        init_ui_kit(light, cx);
        cx.set_reduce_motion(true);
    });

    let telemetry_hub = handle.telemetry().clone();
    let telemetry_source: Arc<dyn TelemetrySource> = Arc::new(telemetry_hub.clone());
    let commands = CommandDispatcher::new(handle.bus().clone(), rt.handle().clone());
    let store: Entity<UiStore> = cx.update(|cx| {
        cx.new(|cx| {
            UiStore::new(
                Arc::clone(&snapshots),
                telemetry_source,
                rt.handle().clone(),
                cx,
            )
        })
    });

    // 1. Open wide (1080x720) window hosting AppShell on RouteId::Overview.
    let wide_shell_ref: Rc<RefCell<Option<WeakEntity<AppShell>>>> = Rc::new(RefCell::new(None));
    let wide_capture = Rc::clone(&wide_shell_ref);
    let wide_window = cx
        .update(|cx| {
            let opts = sized_window_options(1080.0, 720.0, cx);
            let store_c = store.clone();
            let cmds_c = commands.clone();
            MaterialRoot::open_window(opts, cx, move |window, cx| {
                let shell = cx.new(|cx| AppShell::new(store_c, cmds_c, window, cx));
                *wide_capture.borrow_mut() = Some(shell.downgrade());
                shell
            })
        })
        .expect("open wide AppShell window");
    let wide_shell = wide_shell_ref
        .borrow()
        .as_ref()
        .and_then(WeakEntity::upgrade)
        .expect("wide shell");

    // Draw Overview in zh-CN at wide (1080x720) viewport -> two-column layout.
    cx.update_window(wide_window.into(), |_, window, cx| {
        assert!(uses_two_column_layout(f32::from(
            window.viewport_size().width
        )));
        let _ = window.draw(cx);
    })
    .expect("wide zh-CN draw must succeed");
    cx.run_until_parked();

    // AppShell (1) + OverviewView (1) = 2 active Traffic telemetry leases.
    assert_eq!(telemetry_hub.ref_count(TelemetryKind::Traffic), 2);

    // Switch locale to en-US and redraw wide Overview.
    verve_app::i18n::apply_locale("en-US");
    cx.update_window(wide_window.into(), |_, window, cx| {
        wide_shell.update(cx, |_, cx| cx.notify());
        let _ = window.draw(cx);
    })
    .expect("wide en-US draw must succeed");

    // Navigate away from Overview to Profiles: OverviewView drops and releases its Traffic lease!
    cx.update_window(wide_window.into(), |_, window, cx| {
        wide_shell.update(cx, |s, cx| s.navigate(RouteId::Profiles, window, cx));
        let _ = window.draw(cx);
    })
    .expect("navigate away from Overview");
    wait_until(
        cx,
        Duration::from_secs(2),
        "navigating away drops OverviewView Traffic lease",
        |_| telemetry_hub.ref_count(TelemetryKind::Traffic) == 1,
    );

    // Navigate back to Overview: lease increments back to 2.
    cx.update_window(wide_window.into(), |_, window, cx| {
        wide_shell.update(cx, |s, cx| s.navigate(RouteId::Overview, window, cx));
        let _ = window.draw(cx);
    })
    .expect("navigate back to Overview");
    wait_until(
        cx,
        Duration::from_secs(2),
        "navigating back re-acquires OverviewView Traffic lease",
        |_| telemetry_hub.ref_count(TelemetryKind::Traffic) == 2,
    );

    // Release strong handle before closing window so GPUI drops AppShell + OverviewView cleanly.
    drop(wide_shell);
    cx.update_window(wide_window.into(), |_, window, _| {
        window.remove_window();
    })
    .unwrap();
    wait_until(
        cx,
        Duration::from_secs(2),
        "closing wide window releases all Traffic leases",
        |_| telemetry_hub.ref_count(TelemetryKind::Traffic) == 0,
    );
    assert!(
        wide_shell_ref
            .borrow()
            .as_ref()
            .unwrap()
            .upgrade()
            .is_none(),
        "wide AppShell entity must be dropped on window close"
    );

    // 2. Open narrow (760x520) window -> single-column stacked layout in both en-US and zh-CN.
    let narrow_shell_ref: Rc<RefCell<Option<WeakEntity<AppShell>>>> = Rc::new(RefCell::new(None));
    let narrow_capture = Rc::clone(&narrow_shell_ref);
    let narrow_window = cx
        .update(|cx| {
            let opts = sized_window_options(760.0, 520.0, cx);
            let store_c = store.clone();
            let cmds_c = commands.clone();
            MaterialRoot::open_window(opts, cx, move |window, cx| {
                let shell = cx.new(|cx| AppShell::new(store_c, cmds_c, window, cx));
                *narrow_capture.borrow_mut() = Some(shell.downgrade());
                shell
            })
        })
        .expect("open narrow AppShell window");
    let narrow_shell = narrow_shell_ref
        .borrow()
        .as_ref()
        .and_then(WeakEntity::upgrade)
        .expect("narrow shell");

    cx.update_window(narrow_window.into(), |_, window, cx| {
        assert!(!uses_two_column_layout(f32::from(
            window.viewport_size().width
        )));
        let _ = window.draw(cx);
    })
    .expect("narrow en-US draw must succeed");

    verve_app::i18n::apply_locale("zh-CN");
    cx.update_window(narrow_window.into(), |_, window, cx| {
        narrow_shell.update(cx, |_, cx| cx.notify());
        let _ = window.draw(cx);
    })
    .expect("narrow zh-CN draw must succeed");

    // Exercise Direct mode, unavailable service status, and expired subscription rendering branches.
    snapshots.publish_runtime(|runtime| {
        runtime.proxy_mode = ProxyMode::Direct;
        runtime.service_status = ServiceStatus::Unavailable {
            reason: Some("daemon stopped".into()),
        };
    });
    snapshots.publish_profiles(|profiles| {
        if let Some(usage) = profiles.usages.get_mut("remote-main") {
            usage.download_bytes = 95 * 1024 * 1024 * 1024;
            usage.expire_unix_secs = Some(1_700_000_000);
        }
    });
    wait_until(
        cx,
        Duration::from_secs(2),
        "UiStore syncs Direct mode update",
        |cx| cx.read(|cx| store.read(cx).runtime().proxy_mode == ProxyMode::Direct),
    );
    cx.update_window(narrow_window.into(), |_, window, cx| {
        let _ = window.draw(cx);
    })
    .expect("narrow Direct + expired quota draw must succeed");

    // Cleanly close narrow window and shut down coordinator.
    drop(narrow_shell);
    cx.update_window(narrow_window.into(), |_, window, _| {
        window.remove_window();
    })
    .unwrap();
    wait_until(
        cx,
        Duration::from_secs(2),
        "closing narrow window releases all Traffic leases",
        |_| telemetry_hub.ref_count(TelemetryKind::Traffic) == 0,
    );
    assert!(
        narrow_shell_ref
            .borrow()
            .as_ref()
            .unwrap()
            .upgrade()
            .is_none(),
        "narrow AppShell entity must be dropped on window close"
    );

    rt.block_on(async {
        handle.shutdown().await.unwrap();
    });
}
