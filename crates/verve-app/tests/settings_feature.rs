//! Headless `#[gpui::test]` suite for the `0.0.3` Settings feature slice:
//! responsive wide/narrow layouts, bilingual rendering, category switching,
//! inbound/TUN/service commands through `AppCoordinator`, and live Material 3 dynamic seed colors.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{
    AppContext, Bounds, Entity, IntoElement, ParentElement, Render, Styled, TestAppContext, Window,
    WindowBounds, WindowOptions, div, px, size,
};
use gpui_material::MaterialRoot;
use verve_app::features::settings::{SettingsCategory, SettingsView};
use verve_app::{
    AppShell, CommandDispatcher, ConfirmDialogHost, ShellHandle, SideSheetHost, TelemetrySource,
    UiStore,
};
use verve_core::compiler::CompilerPipeline;
use verve_core::config::{ConfigManager, TunConfig};
use verve_core::kernel::{
    ConfigDiagnostic, ControlEndpoint, DirectProcessSupervisor, KernelCapabilities, KernelError,
    KernelSupervisor, ProviderKind, ProxyKernel, ProxyTopology, RemoteKernelStatus, RuleSnapshot,
    ServiceIpcClient, TelemetryKind, TelemetryStream,
};
use verve_core::sys::{AutoLaunchBackend, InMemoryAutoLaunchBackend, InMemorySystemProxyBackend};
use verve_core::{
    AppCoordinator, CoordinatorConfig, CoreError, ServiceControlAction, ServiceControlBackend,
    ServiceStatus,
};
use verve_ui_kit::{
    AppearanceMode, DEFAULT_SEED_ARGB, SEED_COLOR_PRESETS, build_material_theme, init_ui_kit,
    theme_tokens,
};

static LOCALE_TEST_MUTEX: Mutex<()> = Mutex::new(());

#[derive(Clone, Default)]
struct NoopKernel;

impl ProxyKernel for NoopKernel {
    fn capabilities(&self) -> KernelCapabilities {
        KernelCapabilities::default()
    }

    async fn validate_config(&self, _path: &Path) -> Result<(), Vec<ConfigDiagnostic>> {
        Ok(())
    }

    async fn reload_config(&self, _path: &Path) -> Result<(), KernelError> {
        Ok(())
    }

    async fn fetch_proxies(&self) -> Result<ProxyTopology, KernelError> {
        Ok(ProxyTopology {
            groups: Vec::new(),
            nodes: BTreeMap::new(),
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
        Ok(42)
    }

    async fn fetch_rules(&self) -> Result<RuleSnapshot, KernelError> {
        Ok(RuleSnapshot {
            rules: Vec::new(),
            providers: Vec::new(),
        })
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

#[derive(Default)]
struct MockServiceControlBackend {
    installs: AtomicUsize,
    uninstalls: AtomicUsize,
}

impl ServiceControlBackend for MockServiceControlBackend {
    fn install_service<'a>(
        &'a self,
        _trusted_dirs: &'a [PathBuf],
    ) -> Pin<Box<dyn Future<Output = Result<(), CoreError>> + Send + 'a>> {
        self.installs.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }

    fn uninstall_service<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<(), CoreError>> + Send + 'a>> {
        self.uninstalls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

struct MockServiceIpcClient {
    available: AtomicBool,
}

impl MockServiceIpcClient {
    fn new(available: bool) -> Self {
        Self {
            available: AtomicBool::new(available),
        }
    }
}

impl ServiceIpcClient for MockServiceIpcClient {
    fn start_remote<'a>(
        &'a self,
        _kernel_id: &'a str,
        _bin_path: &'a Path,
        _work_dir: &'a Path,
        _config_path: &'a Path,
        _ctrl_endpoint: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<u32, KernelError>> + Send + 'a>> {
        Box::pin(async { Ok(4242) })
    }

    fn stop_remote<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<(), KernelError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    fn query_remote_status<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<RemoteKernelStatus, KernelError>> + Send + 'a>> {
        let ok = self.available.load(Ordering::SeqCst);
        Box::pin(async move {
            if ok {
                Ok(RemoteKernelStatus {
                    running: true,
                    pid: Some(4242),
                    ctrl_endpoint: None,
                })
            } else {
                Err(KernelError::Protocol("service daemon offline".into()))
            }
        })
    }

    fn read_remote_stderr_tail<'a>(
        &'a self,
        _max_lines: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, KernelError>> + Send + 'a>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

struct SettingsTestHost {
    settings: Entity<SettingsView>,
    sheet_host: Entity<SideSheetHost>,
    confirm_host: Entity<ConfirmDialogHost>,
}

impl Render for SettingsTestHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .relative()
            .child(self.settings.clone())
            .child(self.sheet_host.clone())
            .child(self.confirm_host.clone())
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
            "timed out waiting for async settings condition: {label}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn window_options_with_size(width: f32, height: f32, cx: &gpui::App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(width), px(height)),
            cx,
        ))),
        ..Default::default()
    }
}

#[gpui::test]
fn settings_renders_categories_bilingual_and_responsive_widths(cx: &mut TestAppContext) {
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
    let auto_launch_backend = Arc::new(InMemoryAutoLaunchBackend::default());
    let service_control = Arc::new(MockServiceControlBackend::default());
    let service_ipc = Arc::new(MockServiceIpcClient::new(true));

    let mut handle = rt.block_on(async {
        AppCoordinator::spawn(
            cfg_mgr,
            CompilerPipeline::default(),
            KernelSupervisor::Direct(DirectProcessSupervisor::new()),
            Arc::new(NoopKernel),
            CoordinatorConfig {
                control_endpoint: ControlEndpoint::default_for_session("settings-responsive"),
                system_proxy_backend: Some(sysproxy_backend),
                auto_launch_backend: Some(auto_launch_backend),
                service_control_backend: Some(service_control),
                service_ipc_client: Some(service_ipc),
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
    let telemetry_source: Arc<dyn TelemetrySource> = Arc::new(handle.telemetry().clone());
    let commands = CommandDispatcher::new(handle.bus().clone(), rt.handle().clone());
    let store = cx.update(|cx| {
        cx.new(|cx| UiStore::new(snapshots, telemetry_source, rt.handle().clone(), cx))
    });

    // Test both wide (left category rail) and narrow (< 820px horizontal tabs) windows.
    for (width, height) in [(1080.0_f32, 720.0_f32), (720.0_f32, 640.0_f32)] {
        let view_cap: Rc<RefCell<Option<Entity<SettingsView>>>> = Rc::new(RefCell::new(None));
        let view_cap_c = Rc::clone(&view_cap);
        let store_c = store.clone();
        let cmds_c = commands.clone();

        let window = cx
            .update(|cx| {
                let opts = window_options_with_size(width, height, cx);
                MaterialRoot::open_window(opts, cx, move |window, cx| {
                    let shell =
                        cx.new(|cx| AppShell::new(store_c.clone(), cmds_c.clone(), window, cx));
                    let handle: ShellHandle = shell.read(cx).shell_handle().clone();
                    let sheet_host = shell.read(cx).sheet_host().clone();
                    let confirm_host = shell.read(cx).confirm_host().clone();
                    let settings =
                        cx.new(|cx| SettingsView::new(store_c.clone(), cmds_c.clone(), handle, cx));
                    *view_cap_c.borrow_mut() = Some(settings.clone());
                    cx.new(|_| SettingsTestHost {
                        settings,
                        sheet_host,
                        confirm_host,
                    })
                })
            })
            .expect("open settings test window");

        let settings_view = view_cap.borrow().clone().expect("settings view captured");

        for locale in ["zh-CN", "en-US"] {
            verve_app::i18n::apply_locale(locale);
            for category in SettingsCategory::ALL {
                cx.update_window(window.into(), |_, window, cx| {
                    settings_view.update(cx, |v, cx| v.select_category(category, cx));
                    let _ = window.draw(cx);
                })
                .expect("settings draw must succeed");
                assert_eq!(
                    cx.read(|cx| settings_view.read(cx).active_category()),
                    category
                );
            }
        }

        cx.update_window(window.into(), |_, window, _| {
            window.remove_window();
        })
        .unwrap();
        cx.run_until_parked();
    }

    rt.block_on(async {
        handle.shutdown().await.unwrap();
    });
}

#[gpui::test]
fn settings_dispatches_network_tun_service_and_dynamic_seed_commands(cx: &mut TestAppContext) {
    let _locale_guard = LOCALE_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    verve_app::i18n::apply_locale("en-US");
    cx.background_executor.allow_parking();

    let dir = tempfile::tempdir().expect("tempdir");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let cfg_mgr = ConfigManager::open(dir.path()).expect("config manager");
    let sysproxy_backend = Arc::new(InMemorySystemProxyBackend::default());
    let auto_launch_backend = Arc::new(InMemoryAutoLaunchBackend::default());
    let service_control = Arc::new(MockServiceControlBackend::default());
    let service_ipc = Arc::new(MockServiceIpcClient::new(true));

    let mut handle = rt.block_on(async {
        AppCoordinator::spawn(
            cfg_mgr,
            CompilerPipeline::default(),
            KernelSupervisor::Direct(DirectProcessSupervisor::new()),
            Arc::new(NoopKernel),
            CoordinatorConfig {
                control_endpoint: ControlEndpoint::default_for_session("settings-commands"),
                system_proxy_backend: Some(sysproxy_backend),
                auto_launch_backend: Some(
                    Arc::clone(&auto_launch_backend) as Arc<dyn AutoLaunchBackend>
                ),
                service_control_backend: Some(
                    Arc::clone(&service_control) as Arc<dyn ServiceControlBackend>
                ),
                service_ipc_client: Some(Arc::clone(&service_ipc) as Arc<dyn ServiceIpcClient>),
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
    let telemetry_source: Arc<dyn TelemetrySource> = Arc::new(handle.telemetry().clone());
    let commands = CommandDispatcher::new(handle.bus().clone(), rt.handle().clone());
    let store = cx.update(|cx| {
        cx.new(|cx| UiStore::new(snapshots, telemetry_source, rt.handle().clone(), cx))
    });

    let view_cap: Rc<RefCell<Option<Entity<SettingsView>>>> = Rc::new(RefCell::new(None));
    let confirm_cap: Rc<RefCell<Option<Entity<ConfirmDialogHost>>>> = Rc::new(RefCell::new(None));
    let view_cap_c = Rc::clone(&view_cap);
    let confirm_cap_c = Rc::clone(&confirm_cap);
    let store_c = store.clone();
    let cmds_c = commands.clone();

    let window = cx
        .update(|cx| {
            let opts = window_options_with_size(1040.0, 760.0, cx);
            MaterialRoot::open_window(opts, cx, move |window, cx| {
                let shell = cx.new(|cx| AppShell::new(store_c.clone(), cmds_c.clone(), window, cx));
                let shell_handle = shell.read(cx).shell_handle().clone();
                let sheet_host = shell.read(cx).sheet_host().clone();
                let confirm_host = shell.read(cx).confirm_host().clone();
                *confirm_cap_c.borrow_mut() = Some(confirm_host.clone());
                let settings = cx
                    .new(|cx| SettingsView::new(store_c.clone(), cmds_c.clone(), shell_handle, cx));
                *view_cap_c.borrow_mut() = Some(settings.clone());
                cx.new(|_| SettingsTestHost {
                    settings,
                    sheet_host,
                    confirm_host,
                })
            })
        })
        .expect("open settings window");

    let settings_view = view_cap.borrow().clone().expect("settings view");
    let confirm_host = confirm_cap.borrow().clone().expect("confirm host");

    // Initial render initializes TextFieldState entities.
    cx.update_window(window.into(), |_, window, cx| {
        let _ = window.draw(cx);
    })
    .unwrap();

    // 1. Network form: invalid port shows inline error and does not dispatch.
    cx.update_window(window.into(), |_, window, cx| {
        settings_view.update(cx, |v, cx| {
            v.set_mixed_port_draft("0", window, cx);
            assert!(!v.apply_network_form(window, cx));
            assert!(v.mixed_port_error().is_some());
        });
        let _ = window.draw(cx);
    })
    .unwrap();

    // Valid port + multiline/comma bypass list dispatches and persists.
    cx.update_window(window.into(), |_, window, cx| {
        settings_view.update(cx, |v, cx| {
            v.set_mixed_port_draft("10808", window, cx);
            v.set_bypass_draft("localhost, 127.0.0.1\n*.internal.local", window, cx);
            assert!(v.apply_network_form(window, cx));
            assert!(v.mixed_port_error().is_none());
        });
        let _ = window.draw(cx);
    })
    .unwrap();

    wait_until(
        cx,
        Duration::from_secs(2),
        "network settings updated in store",
        |cx| {
            cx.read(|cx| {
                let s = &store.read(cx).settings().syncable;
                s.mixed_port == 10808
                    && s.system_proxy_bypass == vec!["localhost", "127.0.0.1", "*.internal.local"]
            })
        },
    );

    // Reset bypass clears bypass list back to empty system default.
    cx.update_window(window.into(), |_, window, cx| {
        settings_view.update(cx, |v, cx| v.reset_bypass(window, cx));
        let _ = window.draw(cx);
    })
    .unwrap();

    wait_until(
        cx,
        Duration::from_secs(2),
        "bypass list reset to empty",
        |cx| {
            cx.read(|cx| {
                store
                    .read(cx)
                    .settings()
                    .syncable
                    .system_proxy_bypass
                    .is_empty()
            })
        },
    );

    // 2. TUN form: invalid MTU (< 1280) sets inline error; valid fields persist; Safe Defaults restores.
    cx.update_window(window.into(), |_, window, cx| {
        settings_view.update(cx, |v, cx| {
            v.set_tun_mtu_draft("1200", window, cx);
            assert!(!v.apply_tun_form(window, cx));
            assert!(v.tun_mtu_error().is_some());

            v.set_tun_mtu_draft("1500", window, cx);
            v.set_tun_dns_hijack_draft("any:53, tcp://any:53", window, cx);
            v.set_tun_exclude_routes_draft("192.168.0.0/16, 10.0.0.0/8", window, cx);
            v.set_tun_interface_draft("verve-tun0", window, cx);
            assert!(v.apply_tun_form(window, cx));
            assert!(v.tun_mtu_error().is_none());
        });
        let _ = window.draw(cx);
    })
    .unwrap();

    wait_until(
        cx,
        Duration::from_secs(2),
        "TUN settings updated in store",
        |cx| {
            cx.read(|cx| {
                let settings = store.read(cx).settings();
                settings.syncable.tun.mtu == 1500
                    && settings.syncable.tun.dns_hijack == vec!["any:53", "tcp://any:53"]
                    && settings.syncable.tun.route_exclude_address
                        == vec!["192.168.0.0/16", "10.0.0.0/8"]
                    && settings.local.tun_interface_name.as_deref() == Some("verve-tun0")
            })
        },
    );

    cx.update_window(window.into(), |_, window, cx| {
        settings_view.update(cx, |v, cx| v.apply_tun_safe_defaults(window, cx));
        let _ = window.draw(cx);
    })
    .unwrap();

    wait_until(
        cx,
        Duration::from_secs(2),
        "TUN safe defaults applied",
        |cx| {
            cx.read(|cx| {
                let settings = store.read(cx).settings();
                settings.syncable.tun == TunConfig::safe_defaults()
                    && settings.local.tun_interface_name.is_none()
            })
        },
    );

    // 3. Service actions: CheckStatus live-updates runtime.service_status; Install invokes backend;
    //    Uninstall opens destructive confirmation dialog through ShellHandle.
    commands.set_service_mode(true);
    commands.service_action(ServiceControlAction::CheckStatus);
    wait_until(
        cx,
        Duration::from_secs(2),
        "service status probed available",
        |cx| {
            cx.read(|cx| {
                store.read(cx).settings().local.service_mode_enabled
                    && store.read(cx).runtime().service_status
                        == ServiceStatus::Available {
                            running_pid: Some(4242),
                        }
            })
        },
    );

    commands.service_action(ServiceControlAction::Install);
    wait_until(
        cx,
        Duration::from_secs(2),
        "service install called backend",
        |_| service_control.installs.load(Ordering::SeqCst) == 1,
    );

    service_ipc.available.store(false, Ordering::SeqCst);
    commands.service_action(ServiceControlAction::CheckStatus);
    wait_until(
        cx,
        Duration::from_secs(2),
        "service status transitions to unavailable",
        |cx| {
            cx.read(|cx| {
                matches!(
                    store.read(cx).runtime().service_status,
                    ServiceStatus::Unavailable { reason: Some(_) }
                )
            })
        },
    );

    cx.update_window(window.into(), |_, window, cx| {
        let _ = window.draw(cx);
        settings_view.update(cx, |v, cx| v.request_service_uninstall(window, cx));
        let _ = window.draw(cx);
        assert!(confirm_host.read(cx).is_open(cx));
    })
    .unwrap();

    // 4. General & Appearance: dynamic seed color updates theme_tokens(cx).primary live & persists!
    let initial_primary = cx.read(|cx| theme_tokens(cx).primary);
    let terracotta = SEED_COLOR_PRESETS
        .iter()
        .find(|p| p.id == "terracotta")
        .expect("terracotta preset");

    cx.update_window(window.into(), |_, window, cx| {
        settings_view.update(cx, |v, cx| {
            v.select_category(SettingsCategory::General, cx);
            v.apply_seed_color(Some(terracotta.argb), window, cx);
        });
        let _ = window.draw(cx);
    })
    .unwrap();

    let terracotta_primary = cx.read(|cx| theme_tokens(cx).primary);
    assert_ne!(
        initial_primary, terracotta_primary,
        "switching seed preset must immediately update theme_tokens(cx).primary"
    );

    wait_until(
        cx,
        Duration::from_secs(2),
        "terracotta seed color persisted",
        |cx| {
            cx.read(|cx| {
                store.read(cx).settings().syncable.theme_seed_color == Some(terracotta.argb)
            })
        },
    );

    // Invalid custom HEX sets error; valid custom HEX applies and persists; Reset Default restores.
    cx.update_window(window.into(), |_, window, cx| {
        settings_view.update(cx, |v, cx| {
            v.set_custom_seed_hex_draft("not-hex", window, cx);
            assert!(!v.apply_custom_seed_hex(window, cx));
            assert!(v.seed_hex_error().is_some());

            v.set_custom_seed_hex_draft("#4F5B92", window, cx);
            assert!(v.apply_custom_seed_hex(window, cx));
            assert!(v.seed_hex_error().is_none());
        });
        let _ = window.draw(cx);
    })
    .unwrap();

    let indigo_primary = cx.read(|cx| theme_tokens(cx).primary);
    assert_ne!(terracotta_primary, indigo_primary);

    wait_until(
        cx,
        Duration::from_secs(2),
        "custom hex seed color persisted",
        |cx| cx.read(|cx| store.read(cx).settings().syncable.theme_seed_color == Some(0xFF4F_5B92)),
    );

    cx.update_window(window.into(), |_, window, cx| {
        settings_view.update(cx, |v, cx| {
            v.apply_seed_color(None, window, cx);
            v.apply_theme_mode(AppearanceMode::Dark, cx);
        });
        let _ = window.draw(cx);
    })
    .unwrap();

    assert!(cx.read(|cx| theme_tokens(cx).is_dark));
    commands.set_auto_launch(true);

    wait_until(
        cx,
        Duration::from_secs(2),
        "seed reset, dark mode, and auto launch persisted",
        |cx| {
            cx.read(|cx| {
                let s = store.read(cx).settings();
                s.syncable.theme_seed_color.is_none()
                    && s.syncable.theme_mode == "dark"
                    && s.local.auto_launch
            }) && auto_launch_backend.is_enabled().unwrap_or(false)
        },
    );

    // Clean shutdown and verify on-disk persistence in settings.toml & local.toml.
    cx.update_window(window.into(), |_, window, _| {
        window.remove_window();
    })
    .unwrap();
    cx.run_until_parked();

    rt.block_on(async {
        handle.shutdown().await.unwrap();
    });

    let reloaded_mgr = ConfigManager::open(dir.path()).unwrap();
    let reloaded_syncable = reloaded_mgr.load_syncable().unwrap();
    let reloaded_local = reloaded_mgr.load_local().unwrap();
    assert_eq!(reloaded_syncable.mixed_port, 10808);
    assert!(reloaded_syncable.system_proxy_bypass.is_empty());
    assert_eq!(reloaded_syncable.tun, TunConfig::safe_defaults());
    assert_eq!(reloaded_syncable.theme_mode, "dark");
    assert_eq!(reloaded_syncable.theme_seed_color, None);
    assert!(reloaded_local.service_mode_enabled);
    assert!(reloaded_local.auto_launch);
    assert_eq!(reloaded_local.tun_interface_name, None);
}
