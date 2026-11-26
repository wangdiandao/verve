//! Headless `#[gpui::test]` integration suite for the 0.0.3 Profiles feature slice:
//! empty state, remote URL import, local YAML creation & file import sheet,
//! profile cards with quota/expiry/compile diagnostics, detail sheet, and destructive delete dialog
//! across both `zh-CN` and `en-US` locales.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{
    AppContext, ElementId, Entity, InputEvent, IntoElement, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Render, Styled, TestAppContext, Window, div,
};
use gpui_material::MaterialRoot;
use verve_app::features::profiles::{
    DEFAULT_LOCAL_YAML_TEMPLATE, LocalImportMode, NewLocalProfileSheet, ProfilesView,
};
use verve_app::{
    AppShell, CommandDispatcher, ConfirmDialogHost, RouteId, SideSheetHost, SideSheetSpec,
    TelemetrySource, UiStore, default_main_window_options,
};
use verve_core::compiler::CompilerPipeline;
use verve_core::config::ConfigManager;
use verve_core::kernel::{
    ConfigDiagnostic, ControlEndpoint, DirectProcessSupervisor, KernelCapabilities, KernelError,
    KernelSupervisor, ProviderKind, ProxyKernel, ProxyTopology, RuleSnapshot, TelemetryKind,
    TelemetryStream,
};
use verve_core::sys::InMemorySystemProxyBackend;
use verve_core::{
    AppCoordinator, CoordinatorConfig, CoreError, FetchedSubscription, SubscriptionFetcher,
    SubscriptionUserInfo,
};
use verve_ui_kit::{
    AppearanceMode, DEFAULT_SEED_ARGB, apply_theme, build_material_theme, init_ui_kit,
};

static LOCALE_TEST_MUTEX: Mutex<()> = Mutex::new(());

#[derive(Clone, Default)]
struct TestKernel;

impl ProxyKernel for TestKernel {
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

#[derive(Clone)]
struct MockSubscriptionFetcher {
    responses: Arc<Mutex<BTreeMap<String, FetchedSubscription>>>,
}

impl MockSubscriptionFetcher {
    fn new(entries: impl IntoIterator<Item = (String, FetchedSubscription)>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(entries.into_iter().collect())),
        }
    }
}

impl SubscriptionFetcher for MockSubscriptionFetcher {
    fn fetch<'a>(
        &'a self,
        url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<FetchedSubscription, CoreError>> + Send + 'a>> {
        let key = url.trim().to_owned();
        let responses = Arc::clone(&self.responses);
        Box::pin(async move {
            let guard = responses.lock().unwrap();
            guard.get(&key).cloned().ok_or_else(|| {
                CoreError::Subscription(format!("mock subscription URL not found: {key}"))
            })
        })
    }
}

struct ProfilesHarnessRoot {
    _shell: Entity<AppShell>,
    view: Entity<ProfilesView>,
    sheet_host: Entity<SideSheetHost>,
    confirm_host: Entity<ConfirmDialogHost>,
}

impl Render for ProfilesHarnessRoot {
    fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(self.view.clone())
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
            "timed out waiting for async UI/coordinator condition: {label}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn click_observed_element(window: &mut Window, id: impl Into<ElementId>, cx: &mut gpui::App) {
    let target_id = id.into();
    let snapshot = gpui_base::test_support::find(window, &[], &target_id).unwrap_or_else(|| {
        let paths = gpui_base::test_support::registered_paths(window);
        panic!("expected observed element `{target_id:?}` in window; registered paths:\n{paths}");
    });
    let position = snapshot.bounds().center();
    window.dispatch_event(
        MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: Modifiers::none(),
        }
        .to_platform_input(),
        cx,
    );
    window.dispatch_event(
        MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::none(),
            click_count: 1,
            first_mouse: false,
        }
        .to_platform_input(),
        cx,
    );
    window.dispatch_event(
        MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::none(),
            click_count: 1,
        }
        .to_platform_input(),
        cx,
    );
}

#[gpui::test]
fn profiles_empty_state_and_remote_url_import_in_bilingual_locales(cx: &mut TestAppContext) {
    let _locale_guard = LOCALE_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    verve_app::i18n::apply_locale("zh-CN");
    cx.background_executor.allow_parking();

    let dir = tempfile::tempdir().expect("tempdir");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let sub_url = "https://sub.example.com/v1/clash.yaml?token=secret-long-token-0123456789";
    let fetcher = Arc::new(MockSubscriptionFetcher::new([(
        sub_url.to_owned(),
        FetchedSubscription {
            raw_yaml: DEFAULT_LOCAL_YAML_TEMPLATE.to_owned(),
            suggested_name: Some("Cloud Airport".into()),
            usage: Some(SubscriptionUserInfo {
                upload_bytes: 512 * 1024 * 1024,
                download_bytes: 4 * 1024 * 1024 * 1024,
                total_bytes: Some(20 * 1024 * 1024 * 1024),
                expire_unix_secs: Some(2_000_000_000),
            }),
        },
    )]));

    let cfg_mgr = ConfigManager::open(dir.path()).expect("config manager");
    let mut handle = rt.block_on(async {
        AppCoordinator::spawn(
            cfg_mgr,
            CompilerPipeline::default(),
            KernelSupervisor::Direct(DirectProcessSupervisor::new()),
            Arc::new(TestKernel),
            CoordinatorConfig {
                control_endpoint: ControlEndpoint::default_for_session("gpui-test-profiles-empty"),
                system_proxy_backend: Some(Arc::new(InMemorySystemProxyBackend::default())),
                subscription_fetcher: Some(fetcher),
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
    let toast_rx = commands.subscribe_toasts();
    let store = cx.update(|cx| {
        cx.new(|cx| UiStore::new(snapshots, telemetry_source, rt.handle().clone(), cx))
    });

    let view_ref: Rc<RefCell<Option<Entity<ProfilesView>>>> = Rc::new(RefCell::new(None));
    let view_cap = Rc::clone(&view_ref);

    let window = cx
        .update(|cx| {
            let opts = default_main_window_options(cx);
            let store_c = store.clone();
            let cmds_c = commands.clone();
            MaterialRoot::open_window(opts, cx, move |window, cx| {
                let shell = cx.new(|cx| AppShell::new(store_c.clone(), cmds_c.clone(), window, cx));
                let shell_handle = shell.read(cx).shell_handle().clone();
                let sheet_host = shell.read(cx).sheet_host().clone();
                let confirm_host = shell.read(cx).confirm_host().clone();
                let view = cx.new(|cx| ProfilesView::new(store_c, cmds_c, shell_handle, cx));
                *view_cap.borrow_mut() = Some(view.clone());
                cx.new(|_| ProfilesHarnessRoot {
                    _shell: shell,
                    view,
                    sheet_host,
                    confirm_host,
                })
            })
        })
        .expect("open Profiles harness window");

    let view = view_ref.borrow().clone().expect("view captured");

    // 1. Render empty state in zh-CN and en-US.
    for locale in ["zh-CN", "en-US"] {
        verve_app::i18n::apply_locale(locale);
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |_, cx| cx.notify());
            let _ = window.draw(cx);
        })
        .expect("empty state draw must succeed");
    }

    // 2. Reject invalid subscription URL with inline error.
    cx.update_window(window.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.set_remote_url_input("ftp://invalid.example.com/sub", window, cx);
            assert!(!v.submit_remote_url(window, cx));
            assert!(v.url_error().is_some());
        });
        let _ = window.draw(cx);
    })
    .unwrap();

    // 3. Submit valid remote subscription URL and wait for coordinator + UiStore update.
    cx.update_window(window.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.set_remote_url_input(sub_url, window, cx);
            assert!(v.submit_remote_url(window, cx));
            assert!(v.url_error().is_none());
        });
        let _ = window.draw(cx);
    })
    .unwrap();

    let toast = toast_rx
        .try_recv()
        .expect("importing remote URL must emit info toast");
    assert!(!toast.is_error);

    wait_until(
        cx,
        Duration::from_secs(3),
        "remote profile and usage arrive in UiStore",
        |cx| {
            cx.read(|cx| {
                let profiles = store.read(cx).profiles();
                profiles.profiles.len() == 1 && profiles.usages.len() == 1
            })
        },
    );

    // Draw populated remote profile card in both zh-CN and en-US.
    for locale in ["zh-CN", "en-US"] {
        verve_app::i18n::apply_locale(locale);
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |_, cx| cx.notify());
            let _ = window.draw(cx);
        })
        .expect("populated remote profile draw must succeed");
    }

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
fn profiles_local_sheet_detail_sheet_and_delete_confirm_flows(cx: &mut TestAppContext) {
    let _locale_guard = LOCALE_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    verve_app::i18n::apply_locale("zh-CN");
    cx.background_executor.allow_parking();

    let dir = tempfile::tempdir().expect("tempdir");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let active_remote_url = "https://sub.example.com/active.yaml";
    let expired_remote_url = "https://sub.example.com/expired.yaml";
    let unlimited_remote_url = "https://sub.example.com/unlimited.yaml";

    let fetcher = Arc::new(MockSubscriptionFetcher::new([
        (
            active_remote_url.to_owned(),
            FetchedSubscription {
                raw_yaml: DEFAULT_LOCAL_YAML_TEMPLATE.to_owned(),
                suggested_name: Some("Primary Sub".into()),
                usage: Some(SubscriptionUserInfo {
                    upload_bytes: 1_073_741_824,
                    download_bytes: 5_368_709_120,
                    total_bytes: Some(10_737_418_240),
                    expire_unix_secs: Some(2_000_000_000),
                }),
            },
        ),
        (
            expired_remote_url.to_owned(),
            FetchedSubscription {
                raw_yaml: DEFAULT_LOCAL_YAML_TEMPLATE.to_owned(),
                suggested_name: Some("Expired Sub".into()),
                usage: Some(SubscriptionUserInfo {
                    upload_bytes: 9_663_676_416,
                    download_bytes: 2_147_483_648,
                    total_bytes: Some(10_737_418_240),
                    expire_unix_secs: Some(1_600_000_000),
                }),
            },
        ),
        (
            unlimited_remote_url.to_owned(),
            FetchedSubscription {
                raw_yaml: DEFAULT_LOCAL_YAML_TEMPLATE.to_owned(),
                suggested_name: Some("Unlimited Sub".into()),
                usage: Some(SubscriptionUserInfo {
                    upload_bytes: 1024,
                    download_bytes: 2048,
                    total_bytes: None,
                    expire_unix_secs: None,
                }),
            },
        ),
    ]));

    let cfg_mgr = ConfigManager::open(dir.path()).expect("config manager");
    let mut handle = rt.block_on(async {
        AppCoordinator::spawn(
            cfg_mgr,
            CompilerPipeline::default(),
            KernelSupervisor::Direct(DirectProcessSupervisor::new()),
            Arc::new(TestKernel),
            CoordinatorConfig {
                control_endpoint: ControlEndpoint::default_for_session("gpui-test-profiles-full"),
                system_proxy_backend: Some(Arc::new(InMemorySystemProxyBackend::default())),
                subscription_fetcher: Some(fetcher),
                ..Default::default()
            },
        )
        .expect("spawn coordinator")
    });

    // Seed local + remote profiles (finite quota, expired quota, unlimited quota).
    let (primary_remote_id, expired_remote_id) = rt.block_on(async {
        handle
            .bus()
            .import_local_profile(
                "home-local",
                "Home Local",
                DEFAULT_LOCAL_YAML_TEMPLATE,
                None,
            )
            .await
            .unwrap();
        let primary = handle
            .bus()
            .import_remote_profile(active_remote_url, None)
            .await
            .unwrap();
        let expired = handle
            .bus()
            .import_remote_profile(expired_remote_url, None)
            .await
            .unwrap();
        handle
            .bus()
            .import_remote_profile(unlimited_remote_url, None)
            .await
            .unwrap();
        handle.bus().activate_profile(&primary.id).await.unwrap();
        (primary.id, expired.id)
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

    let shell_ref: Rc<RefCell<Option<Entity<AppShell>>>> = Rc::new(RefCell::new(None));
    let view_ref: Rc<RefCell<Option<Entity<ProfilesView>>>> = Rc::new(RefCell::new(None));
    let shell_cap = Rc::clone(&shell_ref);
    let view_cap = Rc::clone(&view_ref);

    let window = cx
        .update(|cx| {
            let opts = default_main_window_options(cx);
            let store_c = store.clone();
            let cmds_c = commands.clone();
            MaterialRoot::open_window(opts, cx, move |window, cx| {
                let shell = cx.new(|cx| AppShell::new(store_c.clone(), cmds_c.clone(), window, cx));
                let shell_handle = shell.read(cx).shell_handle().clone();
                let sheet_host = shell.read(cx).sheet_host().clone();
                let confirm_host = shell.read(cx).confirm_host().clone();
                let view = cx.new(|cx| ProfilesView::new(store_c, cmds_c, shell_handle, cx));
                *shell_cap.borrow_mut() = Some(shell.clone());
                *view_cap.borrow_mut() = Some(view.clone());
                cx.new(|_| ProfilesHarnessRoot {
                    _shell: shell,
                    view,
                    sheet_host,
                    confirm_host,
                })
            })
        })
        .expect("open Profiles full harness window");

    let shell = shell_ref.borrow().clone().expect("shell captured");
    let view = view_ref.borrow().clone().expect("view captured");

    // 1. Render all cards in Light/Dark themes and zh-CN/en-US locales.
    for (locale, mode) in [
        ("zh-CN", AppearanceMode::Light),
        ("en-US", AppearanceMode::Dark),
    ] {
        verve_app::i18n::apply_locale(locale);
        cx.update(|cx| {
            let theme =
                build_material_theme(mode, Some(DEFAULT_SEED_ARGB), mode == AppearanceMode::Dark);
            apply_theme(theme, cx);
        });
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |_, cx| cx.notify());
            let _ = window.draw(cx);
        })
        .expect("profiles cards draw must succeed");
    }

    // 2. Open NewLocalProfileSheet via ProfilesView and verify SideSheetHost opens & draws cleanly.
    cx.update_window(window.into(), |_, window, cx| {
        view.update(cx, |v, cx| v.open_new_local_sheet(window, cx));
        let _ = window.draw(cx);
        assert!(shell.read(cx).sheet_host().read(cx).is_open(cx));
        let handle = shell.read(cx).shell_handle().clone();
        handle.close_sheet(window, cx);
        assert!(!shell.read(cx).sheet_host().read(cx).is_open(cx));
    })
    .unwrap();

    // 3. Test NewLocalProfileSheet: CreateNew validation + valid creation.
    cx.update_window(window.into(), |_, window, cx| {
        let shell_handle = shell.read(cx).shell_handle().clone();
        let sheet = cx.new(|cx| {
            NewLocalProfileSheet::new(
                store.clone(),
                commands.clone(),
                shell_handle.clone(),
                window,
                cx,
            )
        });
        shell_handle.open_sheet(
            SideSheetSpec::new("New Local", sheet.clone().into()),
            window,
            cx,
        );
        let _ = window.draw(cx);

        // Empty name rejected
        sheet.update(cx, |s, cx| {
            assert_eq!(s.mode(), LocalImportMode::CreateNew);
            assert!(!s.submit(window, cx));
            assert!(s.error_message().is_some());
        });

        // Invalid YAML rejected
        sheet.update(cx, |s, cx| {
            s.set_name("Office Direct", window, cx);
            s.set_yaml("- not-a-mapping\n", window, cx);
            assert!(!s.submit(window, cx));
            assert!(s.error_message().is_some());
        });

        // Valid name + starter YAML submits and closes sheet
        sheet.update(cx, |s, cx| {
            s.set_yaml(DEFAULT_LOCAL_YAML_TEMPLATE, window, cx);
            assert!(s.submit(window, cx));
            assert!(s.error_message().is_none());
        });
        let _ = window.draw(cx);
        assert!(!shell.read(cx).sheet_host().read(cx).is_open(cx));
    })
    .unwrap();

    wait_until(
        cx,
        Duration::from_secs(3),
        "created local profile appears in UiStore",
        |cx| {
            cx.read(|cx| {
                store
                    .read(cx)
                    .profiles()
                    .profiles
                    .iter()
                    .any(|p| p.name == "Office Direct")
            })
        },
    );

    // 4. Test NewLocalProfileSheet: ImportFile validation + valid file import from disk.
    let yaml_file_path = dir.path().join("tokyo-custom.yaml");
    std::fs::write(&yaml_file_path, DEFAULT_LOCAL_YAML_TEMPLATE).expect("write temp yaml");
    let bad_yaml_path = dir.path().join("broken.yaml");
    std::fs::write(&bad_yaml_path, "proxies:\n  - [unclosed\n").expect("write bad yaml");

    cx.update_window(window.into(), |_, window, cx| {
        let shell_handle = shell.read(cx).shell_handle().clone();
        let sheet = cx.new(|cx| {
            NewLocalProfileSheet::new(
                store.clone(),
                commands.clone(),
                shell_handle.clone(),
                window,
                cx,
            )
        });
        shell_handle.open_sheet(
            SideSheetSpec::new("Import Local File", sheet.clone().into()),
            window,
            cx,
        );
        sheet.update(cx, |s, cx| {
            s.set_mode(LocalImportMode::ImportFile, cx);
        });
        let _ = window.draw(cx);

        // Empty path rejected
        sheet.update(cx, |s, cx| {
            assert!(!s.submit(window, cx));
            assert!(s.error_message().is_some());
        });

        // Invalid YAML file rejected
        sheet.update(cx, |s, cx| {
            s.set_file_path(&bad_yaml_path.to_string_lossy(), window, cx);
            assert!(!s.submit(window, cx));
            assert!(s.error_message().is_some());
        });

        // Valid YAML file with empty name defaults to file stem ("tokyo-custom")
        sheet.update(cx, |s, cx| {
            s.set_file_path(&yaml_file_path.to_string_lossy(), window, cx);
            assert!(s.submit(window, cx));
            assert!(s.error_message().is_none());
        });
        let _ = window.draw(cx);
        assert!(!shell.read(cx).sheet_host().read(cx).is_open(cx));
    })
    .unwrap();

    wait_until(
        cx,
        Duration::from_secs(3),
        "imported file profile appears with file-stem name",
        |cx| {
            cx.read(|cx| {
                store
                    .read(cx)
                    .profiles()
                    .profiles
                    .iter()
                    .any(|p| p.name == "tokyo-custom")
            })
        },
    );

    // 5. Open ProfileDetailSheet for both remote-with-usage and local profiles across locales.
    for (locale, target_id) in [
        ("zh-CN", primary_remote_id.as_str()),
        ("en-US", expired_remote_id.as_str()),
        ("en-US", "home-local"),
    ] {
        verve_app::i18n::apply_locale(locale);
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |v, cx| {
                assert!(v.open_profile_detail(target_id, window, cx));
            });
            let _ = window.draw(cx);
            assert!(shell.read(cx).sheet_host().read(cx).is_open(cx));
            let handle = shell.read(cx).shell_handle().clone();
            handle.close_sheet(window, cx);
            assert!(!shell.read(cx).sheet_host().read(cx).is_open(cx));
        })
        .unwrap();
    }

    // 6. Open destructive Delete confirmation dialog and click its real "confirm" button!
    assert!(cx.read(|cx| {
        store
            .read(cx)
            .profiles()
            .profiles
            .iter()
            .any(|p| p.id == expired_remote_id)
    }));

    cx.update_window(window.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            assert!(v.open_delete_confirm(&expired_remote_id, window, cx));
        });
        let _ = window.draw(cx);
        assert!(shell.read(cx).confirm_host().read(cx).is_open(cx));

        click_observed_element(window, "confirm", cx);
        let _ = window.draw(cx);
    })
    .unwrap();

    wait_until(
        cx,
        Duration::from_secs(3),
        "confirmed delete removes profile from UiStore",
        |cx| {
            cx.read(|cx| {
                !store
                    .read(cx)
                    .profiles()
                    .profiles
                    .iter()
                    .any(|p| p.id == expired_remote_id)
            })
        },
    );

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
fn profiles_renders_compile_diagnostic_banner_when_activation_fails(cx: &mut TestAppContext) {
    let _locale_guard = LOCALE_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    verve_app::i18n::apply_locale("en-US");
    cx.background_executor.allow_parking();

    let dir = tempfile::tempdir().expect("tempdir");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let cfg_mgr = ConfigManager::open(dir.path()).expect("config manager");
    let mut handle = rt.block_on(async {
        AppCoordinator::spawn(
            cfg_mgr,
            CompilerPipeline::default(),
            KernelSupervisor::Direct(DirectProcessSupervisor::new()),
            Arc::new(TestKernel),
            CoordinatorConfig {
                control_endpoint: ControlEndpoint::default_for_session("gpui-test-profiles-diag"),
                system_proxy_backend: Some(Arc::new(InMemorySystemProxyBackend::default())),
                ..Default::default()
            },
        )
        .expect("spawn coordinator")
    });

    // Import an invalid YAML profile directly and activate it to populate `last_compile_diagnostic`.
    rt.block_on(async {
        handle
            .bus()
            .import_local_profile("bad-yaml", "Broken YAML", "proxies:\n  - [unclosed\n", None)
            .await
            .unwrap();
        let _ = handle.bus().activate_profile("bad-yaml").await;
    });

    assert!(
        handle.snapshot().profiles.last_compile_diagnostic.is_some(),
        "failed activation must populate last_compile_diagnostic"
    );

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

    let shell_ref: Rc<RefCell<Option<Entity<AppShell>>>> = Rc::new(RefCell::new(None));
    let shell_cap = Rc::clone(&shell_ref);

    let window = cx
        .update(|cx| {
            let opts = default_main_window_options(cx);
            let store_c = store.clone();
            let cmds_c = commands.clone();
            MaterialRoot::open_window(opts, cx, move |window, cx| {
                let shell = cx.new(|cx| AppShell::new(store_c, cmds_c, window, cx));
                *shell_cap.borrow_mut() = Some(shell.clone());
                shell
            })
        })
        .expect("open AppShell window");

    let shell = shell_ref.borrow().clone().expect("shell captured");

    // Navigate AppShell to RouteId::Profiles and draw with diagnostic banner in both locales.
    for locale in ["en-US", "zh-CN"] {
        verve_app::i18n::apply_locale(locale);
        cx.update_window(window.into(), |_, window, cx| {
            shell.update(cx, |s, cx| s.navigate(RouteId::Profiles, window, cx));
            let _ = window.draw(cx);
        })
        .expect("Profiles route with diagnostic banner must draw cleanly");
    }

    cx.update_window(window.into(), |_, window, _| {
        window.remove_window();
    })
    .unwrap();
    cx.run_until_parked();

    rt.block_on(async {
        handle.shutdown().await.unwrap();
    });
}
