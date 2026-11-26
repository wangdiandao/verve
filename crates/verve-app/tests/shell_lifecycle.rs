//! Headless `#[gpui::test]` integration suite for `AppShell`, `UiStore`,
//! scoped `SideSheetHost` / `ConfirmDialogHost`, `UiTelemetryLease`, and `TrayController` residency.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{
    AppContext, Entity, IntoElement, ParentElement, Render, TestAppContext, WeakEntity, div,
};
use gpui_material::MaterialRoot;
use verve_app::{
    AppShell, CommandDispatcher, ConfirmDialogSpec, RouteId, SideSheetSpec, TelemetrySource,
    TrayController, UiStore, default_main_window_options,
};

static LOCALE_TEST_MUTEX: Mutex<()> = Mutex::new(());

struct SharedHeadlessTray(Arc<Mutex<HeadlessTrayBackend>>);

impl verve_core::sys::TrayBackend for SharedHeadlessTray {
    fn update_state(
        &mut self,
        state: &verve_core::sys::TrayMenuState,
    ) -> Result<(), verve_core::sys::SysError> {
        self.0.lock().unwrap().update_state(state)
    }
}
use verve_core::compiler::CompilerPipeline;
use verve_core::config::ConfigManager;
use verve_core::kernel::{
    ConfigDiagnostic, ControlEndpoint, DirectProcessSupervisor, KernelCapabilities, KernelError,
    KernelSupervisor, ProviderKind, ProxyGroup, ProxyGroupKind, ProxyKernel, ProxyNode,
    ProxyTopology, RuleEntry, RuleSnapshot, TelemetryEvent, TelemetryKind, TelemetryStream,
    TrafficSample,
};
use verve_core::sys::{HeadlessTrayBackend, InMemorySystemProxyBackend, TrayAction};
use verve_core::{AppCoordinator, CoordinatorConfig};
use verve_ui_kit::{
    AppearanceMode, DEFAULT_SEED_ARGB, apply_theme, build_material_theme, init_ui_kit, theme_tokens,
};

#[derive(Clone, Default)]
struct TestKernel {
    stream_open: Arc<AtomicBool>,
}

impl ProxyKernel for TestKernel {
    fn capabilities(&self) -> KernelCapabilities {
        KernelCapabilities {
            supports_traffic_stream: true,
            supports_memory_stream: true,
            supports_connections_stream: true,
            supports_logs_stream: true,
            ..Default::default()
        }
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
                latest_delay_ms: Some(38),
                alive: true,
            },
        );
        Ok(ProxyTopology {
            groups: vec![ProxyGroup {
                name: "Proxy".into(),
                kind: ProxyGroupKind::Select,
                now: Some("hk-01".into()),
                all: vec!["hk-01".into(), "DIRECT".into()],
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
        Ok(38)
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
            self.stream_open.store(true, Ordering::SeqCst);
            let sample = futures_util::stream::once(async {
                Ok(TelemetryEvent::Traffic(TrafficSample {
                    up_bps: 1_024,
                    down_bps: 8_192,
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

struct DummySheetBody;

impl Render for DummySheetBody {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        div().child("Scoped sheet content")
    }
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
            "timed out waiting for async UI/coordinator condition: {label}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[gpui::test]
fn shell_navigates_all_routes_toggles_theme_and_opens_hosts(cx: &mut TestAppContext) {
    let _locale_guard = LOCALE_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    verve_app::i18n::apply_locale("zh-CN");
    cx.background_executor.allow_parking();
    let dir = tempfile::tempdir().expect("tempdir");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let cfg_mgr = ConfigManager::open(dir.path()).expect("config manager");
    let sysproxy_backend = Arc::new(InMemorySystemProxyBackend::default());
    let kernel = Arc::new(TestKernel::default());

    let mut handle = rt.block_on(async {
        AppCoordinator::spawn(
            cfg_mgr,
            CompilerPipeline::default(),
            KernelSupervisor::Direct(DirectProcessSupervisor::new()),
            Arc::clone(&kernel),
            CoordinatorConfig {
                control_endpoint: ControlEndpoint::default_for_session("gpui-test-nav"),
                system_proxy_backend: Some(sysproxy_backend),
                ..Default::default()
            },
        )
        .expect("spawn coordinator")
    });

    rt.block_on(async {
        handle
            .bus()
            .import_local_profile(
                "demo",
                "Demo Profile",
                "mixed-port: 7890\nrules:\n  - MATCH,DIRECT\n",
                None,
            )
            .await
            .unwrap();
    });

    cx.update(|cx| {
        let light = build_material_theme(AppearanceMode::Light, Some(DEFAULT_SEED_ARGB), false);
        init_ui_kit(light, cx);
        cx.set_reduce_motion(true);
    });

    let snapshots = Arc::clone(handle.snapshots());
    let telemetry_source: Arc<dyn TelemetrySource> = Arc::new(handle.telemetry().clone());
    let commands = CommandDispatcher::new(handle.bus().clone(), rt.handle().clone());

    let store: Entity<UiStore> = cx.update(|cx| {
        cx.new(|cx| UiStore::new(snapshots, telemetry_source, rt.handle().clone(), cx))
    });
    let shell_ref: Rc<RefCell<Option<Entity<AppShell>>>> = Rc::new(RefCell::new(None));
    let shell_capture = Rc::clone(&shell_ref);

    let window = cx
        .update(|cx| {
            let opts = default_main_window_options(cx);
            let store_c = store.clone();
            let cmds_c = commands.clone();
            MaterialRoot::open_window(opts, cx, move |window, cx| {
                let shell = cx.new(|cx| AppShell::new(store_c, cmds_c, window, cx));
                *shell_capture.borrow_mut() = Some(shell.clone());
                shell
            })
        })
        .expect("open AppShell window");

    let shell = shell_ref.borrow().clone().expect("shell captured");

    // Verify all 5 routes render cleanly without panicking.
    for route in RouteId::ALL {
        cx.update_window(window.into(), |_, window, cx| {
            shell.update(cx, |s, cx| s.navigate(route, window, cx));
            let _ = window.draw(cx);
        })
        .expect("route draw must succeed");
        assert_eq!(cx.read(|cx| shell.read(cx).active_route()), route);
    }

    // Verify sidebar collapse toggle.
    cx.update_window(window.into(), |_, window, cx| {
        shell.update(cx, |s, cx| s.toggle_sidebar(cx));
        let _ = window.draw(cx);
    })
    .unwrap();
    assert!(cx.read(|cx| shell.read(cx).is_sidebar_collapsed()));

    // Toggle Light -> Dark theme and verify surface tokens flip.
    let light_surface = cx.read(|cx| theme_tokens(cx).surface);
    cx.update(|cx| {
        let dark = build_material_theme(AppearanceMode::Dark, Some(DEFAULT_SEED_ARGB), true);
        apply_theme(dark, cx);
    });
    cx.run_until_parked();
    let dark_surface = cx.read(|cx| theme_tokens(cx).surface);
    assert_ne!(light_surface, dark_surface);
    assert!(cx.read(|cx| theme_tokens(cx).is_dark));

    // Open and close SideSheetHost & ConfirmDialogHost via ShellHandle.
    let confirmed = Rc::new(AtomicBool::new(false));
    let confirmed_c = Rc::clone(&confirmed);

    cx.update_window(window.into(), |_, window, cx| {
        let handle = shell.read(cx).shell_handle().clone();
        let body = cx.new(|_| DummySheetBody);
        handle.open_sheet(SideSheetSpec::new("Inspect", body.into()), window, cx);
        let _ = window.draw(cx);
        assert!(shell.read(cx).sheet_host().read(cx).is_open(cx));
        handle.close_sheet(window, cx);
        assert!(!shell.read(cx).sheet_host().read(cx).is_open(cx));

        handle.open_confirm(
            ConfirmDialogSpec::new("Confirm", "Proceed?", "Yes", move |_, _| {
                confirmed_c.store(true, Ordering::SeqCst);
            })
            .destructive(true),
            window,
            cx,
        );
        let _ = window.draw(cx);
        assert!(shell.read(cx).confirm_host().read(cx).is_open(cx));
        handle.close_confirm(window, cx);
        assert!(!shell.read(cx).confirm_host().read(cx).is_open(cx));
    })
    .unwrap();

    assert!(!confirmed.load(Ordering::SeqCst));

    cx.update_window(window.into(), |_, window, _| {
        window.remove_window();
    })
    .unwrap();
    cx.run_until_parked();

    rt.block_on(async {
        handle.shutdown().await.unwrap();
    });
}

#[gpui::test]
fn window_close_drops_telemetry_leases_and_tray_reopens_with_live_state(cx: &mut TestAppContext) {
    let _locale_guard = LOCALE_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    verve_app::i18n::apply_locale("zh-CN");
    cx.background_executor.allow_parking();
    let dir = tempfile::tempdir().expect("tempdir");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let cfg_mgr = ConfigManager::open(dir.path()).expect("config manager");
    let sysproxy_backend = Arc::new(InMemorySystemProxyBackend::default());
    let kernel = Arc::new(TestKernel::default());

    let mut handle = rt.block_on(async {
        AppCoordinator::spawn(
            cfg_mgr,
            CompilerPipeline::default(),
            KernelSupervisor::Direct(DirectProcessSupervisor::new()),
            Arc::clone(&kernel),
            CoordinatorConfig {
                control_endpoint: ControlEndpoint::default_for_session("gpui-test-tray"),
                system_proxy_backend: Some(sysproxy_backend.clone()),
                ..Default::default()
            },
        )
        .expect("spawn coordinator")
    });

    cx.update(|cx| {
        let light = build_material_theme(AppearanceMode::Light, Some(DEFAULT_SEED_ARGB), false);
        init_ui_kit(light, cx);
        cx.set_reduce_motion(true);
    });

    let snapshots = Arc::clone(handle.snapshots());
    let telemetry_hub = handle.telemetry().clone();
    let telemetry_source: Arc<dyn TelemetrySource> = Arc::new(telemetry_hub.clone());
    let commands = CommandDispatcher::new(handle.bus().clone(), rt.handle().clone());
    let store = cx.update(|cx| {
        cx.new(|cx| UiStore::new(snapshots, telemetry_source, rt.handle().clone(), cx))
    });

    let (tray_backend, tray_rx) = HeadlessTrayBackend::new();
    let backend_handle = Arc::new(Mutex::new(tray_backend));

    let last_shell: Rc<RefCell<Option<WeakEntity<AppShell>>>> = Rc::new(RefCell::new(None));
    let last_shell_c = Rc::clone(&last_shell);

    let open_store = store.clone();
    let open_cmds = commands.clone();
    let open_window = Rc::new(move |cx: &mut gpui::App| {
        let opts = default_main_window_options(cx);
        let store_c = open_store.clone();
        let cmds_c = open_cmds.clone();
        let cap = Rc::clone(&last_shell_c);
        MaterialRoot::open_window(opts, cx, move |window, cx| {
            let shell = cx.new(|cx| AppShell::new(store_c, cmds_c, window, cx));
            *cap.borrow_mut() = Some(shell.downgrade());
            shell
        })
    });

    let tray = cx.update(|cx| {
        cx.new(|cx| {
            TrayController::new(
                Box::new(SharedHeadlessTray(Arc::clone(&backend_handle))),
                tray_rx,
                store.clone(),
                commands.clone(),
                open_window,
                Some(Rc::new(|_| {})),
                cx,
            )
        })
    });

    // Open initial AppShell on Overview: Traffic stream must be active (AppShell + OverviewView = 2 leases).
    let win1 = cx.update(|cx| {
        tray.update(cx, |ctrl, cx| ctrl.show_or_focus_window(cx))
            .expect("open first window")
    });

    cx.update_window(win1.into(), |_, window, cx| {
        let _ = window.draw(cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert_eq!(telemetry_hub.ref_count(TelemetryKind::Traffic), 2);
    assert!(cx.read(|cx| tray.read(cx).menu_state().window_visible));

    // Close the window: AppShell + OverviewView must drop, releasing all TelemetryLeases (`ref_count == 0`),
    // while AppCoordinator and TrayController remain alive!
    cx.update_window(win1.into(), |_, window, _| {
        window.remove_window();
    })
    .unwrap();
    wait_until(
        cx,
        Duration::from_secs(2),
        "window close drops leases",
        |cx| {
            telemetry_hub.ref_count(TelemetryKind::Traffic) == 0
                && !cx.read(|cx| tray.read(cx).menu_state().window_visible)
        },
    );

    assert!(
        last_shell.borrow().as_ref().unwrap().upgrade().is_none(),
        "AppShell entity must be dropped when window closes"
    );

    // Emit TrayAction::ToggleSystemProxy while no window is open.
    backend_handle
        .lock()
        .unwrap()
        .emit_action(TrayAction::ToggleSystemProxy)
        .unwrap();

    wait_until(
        cx,
        Duration::from_secs(2),
        "toggle system proxy reflected in UiStore",
        |cx| cx.read(|cx| store.read(cx).runtime().system_proxy_enabled),
    );
    assert!(sysproxy_backend.snapshot().enable);
    assert_eq!(sysproxy_backend.snapshot().host, "127.0.0.1");
    assert!(cx.read(|cx| tray.read(cx).menu_state().system_proxy_enabled));

    // Emit TrayAction::ShowWindow: a brand-new AppShell window opens reflecting `system_proxy_enabled == true`.
    backend_handle
        .lock()
        .unwrap()
        .emit_action(TrayAction::ShowWindow)
        .unwrap();

    wait_until(
        cx,
        Duration::from_secs(2),
        "tray ShowWindow reopens window",
        |cx| {
            cx.read(|cx| tray.read(cx).menu_state().window_visible)
                && cx.read(|cx| tray.read(cx).active_window().is_some())
        },
    );

    let win2 = cx
        .read(|cx| tray.read(cx).active_window())
        .expect("reopened window must exist");
    cx.update_window(win2.into(), |_, window, cx| {
        let _ = window.draw(cx);
    })
    .unwrap();
    cx.run_until_parked();

    let reopened_shell = last_shell
        .borrow()
        .as_ref()
        .and_then(|w| w.upgrade())
        .expect("reopened AppShell must be alive");
    assert!(cx.read(|cx| {
        reopened_shell
            .read(cx)
            .store()
            .read(cx)
            .runtime()
            .system_proxy_enabled
    }));
    drop(reopened_shell);
    assert_eq!(telemetry_hub.ref_count(TelemetryKind::Traffic), 2);

    // Cleanly close and shut down coordinator.
    cx.update_window(win2.into(), |_, window, _| {
        window.remove_window();
    })
    .unwrap();
    wait_until(
        cx,
        Duration::from_secs(2),
        "final window close drops leases",
        |_| telemetry_hub.ref_count(TelemetryKind::Traffic) == 0,
    );

    rt.block_on(async {
        handle.shutdown().await.unwrap();
    });
}

#[gpui::test]
fn bilingual_locale_switching_updates_ui_tray_and_persists(cx: &mut TestAppContext) {
    let _locale_guard = LOCALE_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    verve_app::i18n::apply_locale("zh-CN");
    cx.background_executor.allow_parking();

    let dir = tempfile::tempdir().expect("tempdir");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let cfg_mgr = ConfigManager::open(dir.path()).expect("config manager");
    let sysproxy_backend = Arc::new(InMemorySystemProxyBackend::default());
    let kernel = Arc::new(TestKernel::default());

    let mut handle = rt.block_on(async {
        AppCoordinator::spawn(
            cfg_mgr,
            CompilerPipeline::default(),
            KernelSupervisor::Direct(DirectProcessSupervisor::new()),
            Arc::clone(&kernel),
            CoordinatorConfig {
                control_endpoint: ControlEndpoint::default_for_session("gpui-test-i18n"),
                system_proxy_backend: Some(sysproxy_backend),
                ..Default::default()
            },
        )
        .expect("spawn coordinator")
    });

    rt.block_on(async {
        handle
            .bus()
            .import_local_profile(
                "demo",
                "Demo Profile",
                "mixed-port: 7890\nrules:\n  - MATCH,DIRECT\n",
                None,
            )
            .await
            .unwrap();
    });

    cx.update(|cx| {
        let initial_locale = handle.snapshot().settings.syncable.locale.clone();
        verve_app::i18n::apply_locale(&initial_locale);
        let light = build_material_theme(AppearanceMode::Light, Some(DEFAULT_SEED_ARGB), false);
        init_ui_kit(light, cx);
        cx.set_reduce_motion(true);
    });

    let snapshots = Arc::clone(handle.snapshots());
    let telemetry_source: Arc<dyn TelemetrySource> = Arc::new(handle.telemetry().clone());
    let commands = CommandDispatcher::new(handle.bus().clone(), rt.handle().clone());
    let store = cx.update(|cx| {
        cx.new(|cx| UiStore::new(snapshots, telemetry_source, rt.handle().clone(), cx))
    });

    let (tray_backend, tray_rx) = HeadlessTrayBackend::new();
    let backend_handle = Arc::new(Mutex::new(tray_backend));

    let open_store = store.clone();
    let open_cmds = commands.clone();
    let shell_ref: Rc<RefCell<Option<Entity<AppShell>>>> = Rc::new(RefCell::new(None));
    let shell_cap = Rc::clone(&shell_ref);
    let open_window = Rc::new(move |cx: &mut gpui::App| {
        let opts = default_main_window_options(cx);
        let store_c = open_store.clone();
        let cmds_c = open_cmds.clone();
        let cap = Rc::clone(&shell_cap);
        MaterialRoot::open_window(opts, cx, move |window, cx| {
            let shell = cx.new(|cx| AppShell::new(store_c, cmds_c, window, cx));
            *cap.borrow_mut() = Some(shell.clone());
            shell
        })
    });

    let tray = cx.update(|cx| {
        cx.new(|cx| {
            TrayController::new(
                Box::new(SharedHeadlessTray(Arc::clone(&backend_handle))),
                tray_rx,
                store.clone(),
                commands.clone(),
                open_window,
                Some(Rc::new(|_| {})),
                cx,
            )
        })
    });

    let win = cx.update(|cx| {
        tray.update(cx, |ctrl, cx| ctrl.show_or_focus_window(cx))
            .expect("open window")
    });
    let shell = shell_ref.borrow().clone().expect("shell captured");

    // 1. Default startup locale is zh-CN.
    assert_eq!(RouteId::Overview.localized_label(), "概览");
    assert_eq!(
        cx.read(|cx| tray.read(cx).menu_state().labels.mode_rule.clone()),
        "模式：规则"
    );
    assert_eq!(
        cx.read(|cx| tray.read(cx).menu_state().labels.quit.clone()),
        "退出"
    );

    for route in RouteId::ALL {
        cx.update_window(win.into(), |_, window, cx| {
            shell.update(cx, |s, cx| s.navigate(route, window, cx));
            let _ = window.draw(cx);
        })
        .expect("zh-CN route draw must succeed");
    }

    // 2. Switch to en-US and verify live store, global locale, tray labels, and all 5 routes.
    commands.set_locale("en-US");
    wait_until(
        cx,
        Duration::from_secs(2),
        "locale switches to en-US",
        |cx| {
            cx.read(|cx| store.read(cx).settings().syncable.locale == "en-US")
                && cx.read(|cx| tray.read(cx).menu_state().labels.mode_rule == "Mode: Rule")
        },
    );

    assert_eq!(RouteId::Overview.localized_label(), "Overview");
    assert_eq!(RouteId::Settings.localized_label(), "Settings");
    assert_eq!(
        cx.read(|cx| tray.read(cx).menu_state().labels.show_window.clone()),
        "Show Verve"
    );
    assert_eq!(
        cx.read(|cx| tray.read(cx).menu_state().labels.quit.clone()),
        "Quit"
    );

    for route in RouteId::ALL {
        cx.update_window(win.into(), |_, window, cx| {
            shell.update(cx, |s, cx| s.navigate(route, window, cx));
            let _ = window.draw(cx);
        })
        .expect("en-US route draw must succeed");
    }

    // 3. Switch back to zh-CN and verify persistence in settings.toml.
    commands.set_locale("zh-CN");
    wait_until(
        cx,
        Duration::from_secs(2),
        "locale switches back to zh-CN",
        |cx| {
            cx.read(|cx| store.read(cx).settings().syncable.locale == "zh-CN")
                && cx.read(|cx| tray.read(cx).menu_state().labels.mode_rule == "模式：规则")
        },
    );
    assert_eq!(RouteId::Overview.localized_label(), "概览");

    cx.update_window(win.into(), |_, window, _| {
        window.remove_window();
    })
    .unwrap();
    cx.run_until_parked();

    rt.block_on(async {
        handle.shutdown().await.unwrap();
    });

    let reloaded = ConfigManager::open(dir.path())
        .unwrap()
        .load_syncable()
        .unwrap();
    assert_eq!(reloaded.locale, "zh-CN");
}
