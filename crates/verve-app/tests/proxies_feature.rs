//! Headless `#[gpui::test]` integration suite for `ProxiesView` (`0.0.3`):
//! multi-group rendering with zero element ID collisions, bilingual localization,
//! collapse/expand, per-group sort modes, global search filtering, locate-selected,
//! empty states, and real `verve-core` command integration.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{AppContext, Entity, TestAppContext};
use gpui_material::MaterialRoot;
use verve_app::features::proxies::{ProxiesView, ProxySortMode};
use verve_app::{
    AppShell, CommandDispatcher, TelemetrySource, UiStore, default_main_window_options,
};
use verve_core::compiler::CompilerPipeline;
use verve_core::config::ConfigManager;
use verve_core::kernel::{
    ConfigDiagnostic, ControlEndpoint, DirectProcessSupervisor, KernelCapabilities, KernelError,
    KernelSupervisor, ProviderKind, ProxyGroup, ProxyGroupKind, ProxyKernel, ProxyMode, ProxyNode,
    ProxyTopology, RuleSnapshot, TelemetryKind, TelemetryStream,
};
use verve_core::sys::InMemorySystemProxyBackend;
use verve_core::{AppCoordinator, CoordinatorConfig};
use verve_ui_kit::{
    AppearanceMode, DEFAULT_SEED_ARGB, apply_theme, build_material_theme, init_ui_kit,
};

static LOCALE_TEST_MUTEX: Mutex<()> = Mutex::new(());

#[derive(Clone)]
struct MultiGroupTestKernel {
    selected: Arc<Mutex<BTreeMap<String, String>>>,
    tested_nodes: Arc<Mutex<Vec<String>>>,
}

impl Default for MultiGroupTestKernel {
    fn default() -> Self {
        let mut selected = BTreeMap::new();
        selected.insert("Proxy".into(), "hk-01".into());
        selected.insert("Auto".into(), "jp-01".into());
        selected.insert("Streaming".into(), "us-01".into());
        Self {
            selected: Arc::new(Mutex::new(selected)),
            tested_nodes: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl MultiGroupTestKernel {
    fn build_topology(&self) -> ProxyTopology {
        let selected = self.selected.lock().unwrap();
        let mut nodes = BTreeMap::new();
        nodes.insert(
            "hk-01".into(),
            ProxyNode {
                name: "hk-01".into(),
                protocol: "vless".into(),
                udp: true,
                latest_delay_ms: Some(42),
                alive: true,
            },
        );
        nodes.insert(
            "jp-01".into(),
            ProxyNode {
                name: "jp-01".into(),
                protocol: "Shadowsocks".into(),
                udp: true,
                latest_delay_ms: Some(26),
                alive: true,
            },
        );
        nodes.insert(
            "us-01".into(),
            ProxyNode {
                name: "us-01".into(),
                protocol: "trojan".into(),
                udp: false,
                latest_delay_ms: Some(185),
                alive: true,
            },
        );
        nodes.insert(
            "sg-untested".into(),
            ProxyNode {
                name: "sg-untested".into(),
                protocol: "vmess".into(),
                udp: false,
                latest_delay_ms: None,
                alive: true,
            },
        );

        ProxyTopology {
            groups: vec![
                ProxyGroup {
                    name: "Proxy".into(),
                    kind: ProxyGroupKind::Select,
                    now: selected.get("Proxy").cloned(),
                    all: vec![
                        "hk-01".into(),
                        "jp-01".into(),
                        "us-01".into(),
                        "sg-untested".into(),
                        "DIRECT".into(),
                    ],
                    hidden: false,
                },
                ProxyGroup {
                    name: "Auto".into(),
                    kind: ProxyGroupKind::UrlTest,
                    now: selected.get("Auto").cloned(),
                    all: vec!["hk-01".into(), "jp-01".into(), "us-01".into()],
                    hidden: false,
                },
                ProxyGroup {
                    name: "Streaming".into(),
                    kind: ProxyGroupKind::Fallback,
                    now: selected.get("Streaming").cloned(),
                    all: vec!["us-01".into(), "hk-01".into()],
                    hidden: false,
                },
                ProxyGroup {
                    name: "HiddenInternal".into(),
                    kind: ProxyGroupKind::Select,
                    now: Some("DIRECT".into()),
                    all: vec!["DIRECT".into()],
                    hidden: true,
                },
            ],
            nodes,
        }
    }
}

impl ProxyKernel for MultiGroupTestKernel {
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
        Ok(self.build_topology())
    }

    async fn select_proxy(&self, group: &str, name: &str) -> Result<(), KernelError> {
        self.selected
            .lock()
            .unwrap()
            .insert(group.to_owned(), name.to_owned());
        Ok(())
    }

    async fn test_delay(
        &self,
        name: &str,
        _url: &str,
        _timeout: Duration,
    ) -> Result<u16, KernelError> {
        self.tested_nodes.lock().unwrap().push(name.to_owned());
        let delay = match name {
            "jp-01" => 24,
            "hk-01" => 39,
            "us-01" => 172,
            _ => 95,
        };
        Ok(delay)
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
            "timed out waiting for async condition: {label}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[gpui::test]
fn proxies_view_renders_multi_group_grid_search_sort_collapse_locate_and_bilingual(
    cx: &mut TestAppContext,
) {
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
    let kernel = Arc::new(MultiGroupTestKernel::default());

    let mut handle = rt.block_on(async {
        AppCoordinator::spawn(
            cfg_mgr,
            CompilerPipeline::default(),
            KernelSupervisor::Direct(DirectProcessSupervisor::new()),
            Arc::clone(&kernel),
            CoordinatorConfig {
                control_endpoint: ControlEndpoint::default_for_session("gpui-test-proxies"),
                system_proxy_backend: Some(sysproxy_backend),
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

    let store: Entity<UiStore> = cx.update(|cx| {
        cx.new(|cx| UiStore::new(snapshots.clone(), telemetry_source, rt.handle().clone(), cx))
    });

    let view_ref: Rc<RefCell<Option<Entity<ProxiesView>>>> = Rc::new(RefCell::new(None));
    let view_cap = Rc::clone(&view_ref);

    let window = cx
        .update(|cx| {
            let opts = default_main_window_options(cx);
            let store_c = store.clone();
            let cmds_c = commands.clone();
            MaterialRoot::open_window(opts, cx, move |window, cx| {
                let shell_entity =
                    cx.new(|cx| AppShell::new(store_c.clone(), cmds_c.clone(), window, cx));
                let shell = shell_entity.read(cx).shell_handle().clone();
                let view = cx.new(|cx| ProxiesView::new(store_c, cmds_c, shell, cx));
                *view_cap.borrow_mut() = Some(view.clone());
                view
            })
        })
        .expect("open ProxiesView window");

    let view = view_ref.borrow().clone().expect("ProxiesView captured");

    // 1. Render initial empty state (kernel stopped) cleanly.
    cx.update_window(window.into(), |_, window, cx| {
        let _ = window.draw(cx);
    })
    .expect("stopped empty state draw must succeed");

    // 1b. Render kernel-running empty state (no groups loaded yet) cleanly.
    snapshots.publish_runtime(|rt_slice| {
        rt_slice.kernel_running = true;
        rt_slice.proxy_mode = ProxyMode::Rule;
    });
    wait_until(
        cx,
        Duration::from_secs(2),
        "UiStore reflects kernel running",
        |cx| cx.read(|cx| store.read(cx).runtime().kernel_running),
    );
    cx.update_window(window.into(), |_, window, cx| {
        let _ = window.draw(cx);
    })
    .expect("running empty state draw must succeed");

    // 2. Populate multi-group topology (with overlapping node names across 3 visible groups + 1 hidden)
    //    and verify zero GPUI element ID collisions!
    let topo = kernel.build_topology();
    snapshots.publish_proxies(|p| {
        p.groups = topo.groups;
        p.nodes = topo.nodes;
    });

    wait_until(
        cx,
        Duration::from_secs(2),
        "UiStore receives multi-group proxies",
        |cx| cx.read(|cx| store.read(cx).proxies().groups.len() == 4),
    );

    // Draw in zh-CN Light mode.
    cx.update_window(window.into(), |_, window, cx| {
        let _ = window.draw(cx);
    })
    .expect("zh-CN multi-group draw must succeed with zero ID collisions");

    // 3. Switch to en-US and Dark mode, draw again.
    verve_app::i18n::apply_locale("en-US");
    cx.update(|cx| {
        let dark = build_material_theme(AppearanceMode::Dark, Some(DEFAULT_SEED_ARGB), true);
        apply_theme(dark, cx);
    });
    cx.update_window(window.into(), |_, window, cx| {
        let _ = window.draw(cx);
    })
    .expect("en-US dark multi-group draw must succeed");

    // 4. Test per-group collapse & expand.
    cx.update_window(window.into(), |_, window, cx| {
        assert!(!view.read(cx).is_group_collapsed("Proxy"));
        view.update(cx, |v, cx| v.toggle_group_collapsed("Proxy", cx));
        assert!(view.read(cx).is_group_collapsed("Proxy"));
        assert!(!view.read(cx).is_group_collapsed("Auto"));
        let _ = window.draw(cx);

        view.update(cx, |v, cx| v.toggle_group_collapsed("Proxy", cx));
        assert!(!view.read(cx).is_group_collapsed("Proxy"));
        let _ = window.draw(cx);
    })
    .expect("collapse/expand draw must succeed");

    // 5. Test per-group sort modes (Default -> Latency -> Name -> Default).
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(
            view.read(cx).group_sort_mode("Proxy"),
            ProxySortMode::Default
        );
        view.update(cx, |v, cx| v.cycle_group_sort_mode("Proxy", cx));
        assert_eq!(
            view.read(cx).group_sort_mode("Proxy"),
            ProxySortMode::Latency
        );
        let _ = window.draw(cx);

        view.update(cx, |v, cx| v.cycle_group_sort_mode("Proxy", cx));
        assert_eq!(view.read(cx).group_sort_mode("Proxy"), ProxySortMode::Name);
        let _ = window.draw(cx);

        view.update(cx, |v, cx| {
            v.set_group_sort_mode("Proxy", ProxySortMode::Default, cx)
        });
        assert_eq!(
            view.read(cx).group_sort_mode("Proxy"),
            ProxySortMode::Default
        );
        let _ = window.draw(cx);
    })
    .expect("sort mode transitions must draw cleanly");

    // 6. Test global search filtering (partial match + zero-match empty state + clear search).
    cx.update_window(window.into(), |_, window, cx| {
        view.update(cx, |v, cx| v.set_search_query("trojan", window, cx));
        assert_eq!(view.read(cx).search_query(), "trojan");
        let _ = window.draw(cx);

        // Zero matching nodes -> renders search empty state.
        view.update(cx, |v, cx| {
            v.set_search_query("zzz-no-such-node", window, cx)
        });
        let _ = window.draw(cx);

        view.update(cx, |v, cx| v.clear_search(window, cx));
        assert!(view.read(cx).search_query().is_empty());
        let _ = window.draw(cx);
    })
    .expect("search filtering & empty state must draw cleanly");

    // 7. Test locate-selected: collapses group + hides selected node via search, then locates it.
    cx.update_window(window.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.toggle_group_collapsed("Proxy", cx);
            v.set_search_query("trojan", window, cx);
        });
        assert!(view.read(cx).is_group_collapsed("Proxy"));
        assert_eq!(view.read(cx).search_query(), "trojan");

        let located = view.update(cx, |v, cx| v.locate_selected_node("Proxy", window, cx));
        assert_eq!(located.as_deref(), Some("hk-01"));
        assert!(!view.read(cx).is_group_collapsed("Proxy"));
        assert!(
            view.read(cx).search_query().is_empty(),
            "locate must clear a search filter that hides the active node"
        );
        assert_eq!(view.read(cx).highlighted_target(), Some(("Proxy", "hk-01")));
        let _ = window.draw(cx);
    })
    .expect("locate selected node must expand, clear hiding filter, highlight, and draw");

    let toast = toast_rx
        .try_recv()
        .expect("locate_selected_node must emit an info toast");
    assert!(!toast.is_error);
    assert!(toast.message.contains("hk-01"));

    // 8. Test real coordinator command: refresh_proxies + test_all_groups_delay + select_proxy.
    commands.select_proxy("Proxy", "jp-01");
    wait_until(
        cx,
        Duration::from_secs(2),
        "select_proxy updates Proxy group active node",
        |cx| {
            cx.read(|cx| {
                store
                    .read(cx)
                    .proxies()
                    .groups
                    .iter()
                    .find(|g| g.name == "Proxy")
                    .and_then(|g| g.now.as_deref())
                    == Some("jp-01")
            })
        },
    );

    commands.test_all_groups_delay(
        "http://www.gstatic.com/generate_204",
        Duration::from_secs(5),
    );
    wait_until(
        cx,
        Duration::from_secs(2),
        "test_all_groups_delay probes leaf nodes",
        |_| !kernel.tested_nodes.lock().unwrap().is_empty(),
    );

    cx.update_window(window.into(), |_, window, cx| {
        let _ = window.draw(cx);
    })
    .expect("post-command redraw must succeed");

    // 9. Test Direct mode empty state.
    snapshots.publish_runtime(|rt_slice| {
        rt_slice.proxy_mode = ProxyMode::Direct;
    });
    wait_until(
        cx,
        Duration::from_secs(2),
        "UiStore reflects Direct mode",
        |cx| cx.read(|cx| store.read(cx).runtime().proxy_mode == ProxyMode::Direct),
    );
    cx.update_window(window.into(), |_, window, cx| {
        let _ = window.draw(cx);
    })
    .expect("Direct mode empty state must draw cleanly");

    // Restore default locale and shut down cleanly.
    verve_app::i18n::apply_locale("zh-CN");
    cx.update_window(window.into(), |_, window, _| {
        window.remove_window();
    })
    .unwrap();
    cx.run_until_parked();

    rt.block_on(async {
        handle.shutdown().await.unwrap();
    });
}
