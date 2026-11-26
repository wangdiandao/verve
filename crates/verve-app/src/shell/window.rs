//! Main desktop application window root ([`AppShell`]).

use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Entity, FontWeight, IntoElement, ParentElement,
    Render, Styled, Subscription, Window, WindowAppearance, WindowBounds, WindowOptions, div, px,
    size,
};
use gpui_material::window_chrome::{TitleBar, title_bar_window_options_with_height};
use gpui_material::{
    Badge, Icon, IconButton, IconName, NavigationItem, NavigationItems, NavigationRail,
    NavigationRailColors, NavigationRailState, RailMode, Scaffold, SemanticTone, Snackbar,
    SnackbarHost,
};
use verve_core::DomainKind;
use verve_core::kernel::TelemetryKind;
use verve_ui_kit::{
    AppearanceMode, DEFAULT_SEED_ARGB, HealthIndicator, KernelHealthState, MiniSparkline,
    TrafficDataPoint, apply_theme, build_material_theme, format_bytes, format_throughput_bps,
    theme_tokens,
};

use crate::bridge::{CommandDispatcher, DomainChanged, UiStore, UiTelemetryLease};
use crate::features::activity::ActivityView;
use crate::features::overview::OverviewView;
use crate::features::profiles::ProfilesView;
use crate::features::proxies::ProxiesView;
use crate::features::settings::SettingsView;
use crate::shell::nav::{RouteId, route_registry};
use crate::shell::sheet_host::{ConfirmDialogHost, ShellHandle, SideSheetHost};

const TITLE_BAR_HEIGHT_PX: f32 = 40.0;

/// Returns default [`WindowOptions`] configured for Verve's 40px client-drawn [`TitleBar`].
pub fn default_main_window_options(cx: &App) -> WindowOptions {
    let base = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1080.0), px(720.0)),
            cx,
        ))),
        window_min_size: Some(size(px(760.0), px(520.0))),
        app_id: Some("io.github.wangdiandao.verve".into()),
        ..Default::default()
    };

    let mut options = title_bar_window_options_with_height(base, px(TITLE_BAR_HEIGHT_PX));
    if let Some(ref mut titlebar) = options.titlebar {
        titlebar.title = Some("Verve".into());
    }
    options
}

enum ActiveRouteView {
    Overview(Entity<OverviewView>),
    Proxies(Entity<ProxiesView>),
    Profiles(Entity<ProfilesView>),
    Activity(Entity<ActivityView>),
    Settings(Entity<SettingsView>),
}

impl ActiveRouteView {
    fn into_any_element(self_ref: &Self) -> AnyElement {
        match self_ref {
            Self::Overview(view) => view.clone().into_any_element(),
            Self::Proxies(view) => view.clone().into_any_element(),
            Self::Profiles(view) => view.clone().into_any_element(),
            Self::Activity(view) => view.clone().into_any_element(),
            Self::Settings(view) => view.clone().into_any_element(),
        }
    }

    fn notify(&self, cx: &mut App) {
        match self {
            Self::Overview(view) => view.update(cx, |_, cx| cx.notify()),
            Self::Proxies(view) => view.update(cx, |_, cx| cx.notify()),
            Self::Profiles(view) => view.update(cx, |_, cx| cx.notify()),
            Self::Activity(view) => view.update(cx, |_, cx| cx.notify()),
            Self::Settings(view) => view.update(cx, |_, cx| cx.notify()),
        }
    }
}

/// Root window view hosting the 40px TitleBar, animated expandable/collapsible NavigationRail,
/// bottom telemetry pod, active feature outlet, scoped sheet/dialog hosts, and error Snackbar.
pub struct AppShell {
    store: Entity<UiStore>,
    commands: CommandDispatcher,
    active_route: RouteId,
    active_view: ActiveRouteView,
    sidebar_collapsed: bool,
    rail_state: Entity<NavigationRailState>,
    sheet_host: Entity<SideSheetHost>,
    confirm_host: Entity<ConfirmDialogHost>,
    snackbar_host: Entity<SnackbarHost>,
    shell_handle: ShellHandle,
    _telemetry_lease: UiTelemetryLease,
    _store_sub: Subscription,
    _rail_obs: Subscription,
    _error_task: gpui::Task<()>,
}

impl AppShell {
    /// Mounts a fresh [`AppShell`] for a window, acquiring a scoped sidebar `Traffic` lease.
    pub fn new(
        store: Entity<UiStore>,
        commands: CommandDispatcher,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        window.on_window_should_close(cx, |_, _| true);

        let rail_state = cx.new(|_| NavigationRailState::new(RailMode::Expanded));
        let rail_obs = cx.observe(&rail_state, |_, _, cx| cx.notify());

        let sheet_host = cx.new(SideSheetHost::new);
        let confirm_host = cx.new(ConfirmDialogHost::new);
        let snackbar_host = cx.new(|_| SnackbarHost::default());

        let weak_shell = cx.weak_entity();
        let shell_handle = ShellHandle::new(
            sheet_host.downgrade(),
            confirm_host.downgrade(),
            move |route, window, cx| {
                let _ = weak_shell.update(cx, |shell, cx| {
                    shell.navigate(route, window, cx);
                });
            },
        );

        let initial_route = RouteId::Overview;
        let active_view =
            Self::build_route_view(initial_route, &store, &commands, &shell_handle, cx);
        let telemetry_lease = store.read(cx).acquire_telemetry(TelemetryKind::Traffic);

        let store_sub = cx.subscribe(&store, |this, store, ev: &DomainChanged, cx| match ev.0 {
            DomainKind::Runtime | DomainKind::Telemetry | DomainKind::Activity => {
                cx.notify();
            }
            DomainKind::Settings => {
                let syncable = store.read(cx).settings().syncable.clone();
                if let Some(mode) = AppearanceMode::from_config_str(&syncable.theme_mode) {
                    let system_dark = matches!(
                        cx.window_appearance(),
                        WindowAppearance::Dark | WindowAppearance::VibrantDark
                    );
                    apply_theme(
                        build_material_theme(mode, Some(DEFAULT_SEED_ARGB), system_dark),
                        cx,
                    );
                }
                crate::i18n::apply_locale(&syncable.locale);
                this.active_view.notify(cx);
                this.sheet_host.update(cx, |_, cx| cx.notify());
                this.confirm_host.update(cx, |_, cx| cx.notify());
                cx.notify();
            }
            DomainKind::Proxies | DomainKind::Profiles => {}
        });

        let error_rx = commands.subscribe_errors();
        let snackbar_weak = snackbar_host.downgrade();
        let error_task = window.spawn(cx, async move |cx| {
            while let Ok(message) = error_rx.recv().await {
                let updated = cx.update(|window, cx| {
                    let _ = snackbar_weak.update(cx, |host, cx| {
                        host.push(
                            Snackbar::new(message).tone(SemanticTone::Danger),
                            window,
                            cx,
                        );
                    });
                });
                if updated.is_err() {
                    break;
                }
            }
        });

        Self {
            store,
            commands,
            active_route: initial_route,
            active_view,
            sidebar_collapsed: false,
            rail_state,
            sheet_host,
            confirm_host,
            snackbar_host,
            shell_handle,
            _telemetry_lease: telemetry_lease,
            _store_sub: store_sub,
            _rail_obs: rail_obs,
            _error_task: error_task,
        }
    }

    /// Returns the currently active [`RouteId`].
    pub fn active_route(&self) -> RouteId {
        self.active_route
    }

    /// Returns `true` when the sidebar is collapsed into compact [`NavigationRail`] mode.
    pub fn is_sidebar_collapsed(&self) -> bool {
        self.sidebar_collapsed
    }

    /// Toggles [`NavigationRail`] between [`RailMode::Expanded`] and [`RailMode::Collapsed`]
    /// with a smooth width transition.
    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        let target = if self.sidebar_collapsed {
            RailMode::Collapsed
        } else {
            RailMode::Expanded
        };
        self.rail_state.update(cx, |state, cx| {
            state.animate_to(target, cx);
        });
        cx.notify();
    }

    /// Switches the active route, dropping the previous feature entity (and its telemetry leases).
    pub fn navigate(&mut self, route: RouteId, _window: &mut Window, cx: &mut Context<Self>) {
        if self.active_route == route {
            return;
        }
        self.active_route = route;
        self.active_view =
            Self::build_route_view(route, &self.store, &self.commands, &self.shell_handle, cx);
        cx.notify();
    }

    /// Returns the shell capability handle passed to feature slices.
    pub fn shell_handle(&self) -> &ShellHandle {
        &self.shell_handle
    }

    /// Returns the scoped [`SideSheetHost`] entity.
    pub fn sheet_host(&self) -> &Entity<SideSheetHost> {
        &self.sheet_host
    }

    /// Returns the scoped [`ConfirmDialogHost`] entity.
    pub fn confirm_host(&self) -> &Entity<ConfirmDialogHost> {
        &self.confirm_host
    }

    /// Returns the underlying [`UiStore`] entity.
    pub fn store(&self) -> &Entity<UiStore> {
        &self.store
    }

    fn build_route_view(
        route: RouteId,
        store: &Entity<UiStore>,
        commands: &CommandDispatcher,
        shell: &ShellHandle,
        cx: &mut Context<Self>,
    ) -> ActiveRouteView {
        let store = store.clone();
        let commands = commands.clone();
        let shell = shell.clone();

        match route {
            RouteId::Overview => ActiveRouteView::Overview(
                cx.new(|cx| OverviewView::new(store, commands, shell, cx)),
            ),
            RouteId::Proxies => {
                ActiveRouteView::Proxies(cx.new(|cx| ProxiesView::new(store, commands, cx)))
            }
            RouteId::Profiles => ActiveRouteView::Profiles(
                cx.new(|cx| ProfilesView::new(store, commands, shell, cx)),
            ),
            RouteId::Activity => {
                ActiveRouteView::Activity(cx.new(|cx| ActivityView::new(store, commands, cx)))
            }
            RouteId::Settings => {
                ActiveRouteView::Settings(cx.new(|cx| SettingsView::new(store, commands, cx)))
            }
        }
    }

    fn render_telemetry_pod(&self, collapsed: bool, cx: &mut Context<Self>) -> AnyElement {
        let tokens = theme_tokens(cx);
        let telemetry = self.store.read(cx).telemetry();
        let down_text = format_throughput_bps(telemetry.download_bps);
        let up_text = format_throughput_bps(telemetry.upload_bps);

        if collapsed {
            return div()
                .w_full()
                .px(px(6.0))
                .py(px(8.0))
                .flex()
                .justify_center()
                .child(
                    div()
                        .px(px(8.0))
                        .py(px(4.0))
                        .rounded_full()
                        .bg(tokens.surface_container_high)
                        .border_1()
                        .border_color(tokens.outline_variant)
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(tokens.primary)
                        .child(format!("↓ {down_text}")),
                )
                .into_any_element();
        }

        let points: Vec<TrafficDataPoint> = telemetry
            .traffic_history
            .iter()
            .map(|s| TrafficDataPoint {
                up_bps: s.up_bps,
                down_bps: s.down_bps,
            })
            .collect();

        div()
            .w_full()
            .p(px(10.0))
            .rounded(px(12.0))
            .bg(tokens.surface_container)
            .border_1()
            .border_color(tokens.outline_variant)
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(tokens.primary)
                            .child(format!("↓ {down_text}")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.tertiary)
                            .child(format!("↑ {up_text}")),
                    ),
            )
            .child(MiniSparkline::new(points).height(px(40.0)))
            .child(
                div().text_xs().text_color(tokens.on_surface_variant).child(
                    rust_i18n::t!(
                        "shell.telemetry.ram",
                        value = format_bytes(telemetry.memory_bytes)
                    )
                    .into_owned(),
                ),
            )
            .into_any_element()
    }
}

impl Render for AppShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let store = self.store.read(cx);
        let runtime = store.runtime().clone();
        let conn_count = store.activity().connections.len();

        let shell_entity = cx.weak_entity();
        let collapse_btn = IconButton::new(
            "verve-sidebar-toggle",
            if self.sidebar_collapsed {
                rust_i18n::t!("shell.expand_nav").into_owned()
            } else {
                rust_i18n::t!("shell.collapse_nav").into_owned()
            },
            Icon::new(if self.sidebar_collapsed {
                IconName::LeftPanelOpen
            } else {
                IconName::LeftPanelClose
            }),
        )
        .on_activate(move |_, cx| {
            let _ = shell_entity.update(cx, |shell, cx| shell.toggle_sidebar(cx));
        });

        let theme_cmd = self.commands.clone();
        let is_dark = tokens.is_dark;
        let theme_btn = IconButton::new(
            "verve-theme-toggle",
            if is_dark {
                rust_i18n::t!("shell.switch_to_light").into_owned()
            } else {
                rust_i18n::t!("shell.switch_to_dark").into_owned()
            },
            Icon::new(if is_dark {
                IconName::LightMode
            } else {
                IconName::DarkMode
            }),
        )
        .on_activate(move |_, cx| {
            let next_mode = if is_dark {
                AppearanceMode::Light
            } else {
                AppearanceMode::Dark
            };
            let system_dark = matches!(
                cx.window_appearance(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            );
            apply_theme(
                build_material_theme(next_mode, Some(DEFAULT_SEED_ARGB), system_dark),
                cx,
            );
            theme_cmd.set_theme_mode(next_mode.as_config_str());
        });

        let (health_state, health_label) = if runtime.kernel_running {
            (
                KernelHealthState::Running,
                rust_i18n::t!("shell.health.running", port = runtime.mixed_port).into_owned(),
            )
        } else if runtime.last_error.is_some() {
            (
                KernelHealthState::Fault,
                rust_i18n::t!("shell.health.fault").into_owned(),
            )
        } else {
            (
                KernelHealthState::Stopped,
                rust_i18n::t!("shell.health.stopped").into_owned(),
            )
        };

        let title_bar = TitleBar::new("verve-title-bar")
            .height(px(TITLE_BAR_HEIGHT_PX))
            .title("Verve")
            .leading(collapse_btn)
            .trailing(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .pr(px(8.0))
                    .child(theme_btn)
                    .child(HealthIndicator::new(health_state).label(health_label)),
            );

        let nav_items_vec: Vec<NavigationItem> = route_registry()
            .iter()
            .map(|desc| {
                let mut item = NavigationItem::new(desc.id, desc.route.localized_label())
                    .icon(desc.route.icon());
                if desc.route == RouteId::Activity && conn_count > 0 {
                    item = item.badge(Badge::standalone_count(conn_count, 99));
                }
                item
            })
            .collect();

        let nav_entity = cx.weak_entity();
        let nav_items = NavigationItems::new(
            "verve-primary-nav",
            rust_i18n::t!("shell.primary_nav").into_owned(),
            nav_items_vec,
        )
        .selected(self.active_route.id_str())
        .on_select(move |id, window, cx| {
            if let Some(route) = RouteId::from_id_str(id.as_ref()) {
                let _ = nav_entity.update(cx, |shell, cx| {
                    shell.navigate(route, window, cx);
                });
            }
        });

        let sidebar = div()
            .h_full()
            .flex()
            .flex_col()
            .justify_between()
            .overflow_hidden()
            .bg(tokens.surface_container_low)
            .border_r_1()
            .border_color(tokens.outline_variant)
            .child(
                div().flex_1().min_h_0().child(
                    NavigationRail::new("verve-nav-rail", nav_items)
                        .state(&self.rail_state)
                        .expanded_width(px(236.0))
                        .title(rust_i18n::t!("shell.primary_nav").into_owned())
                        .colors(NavigationRailColors {
                            container: Some(tokens.surface_container_low),
                            ..Default::default()
                        }),
                ),
            )
            .child(
                div()
                    .w(px(0.0))
                    .min_w_full()
                    .overflow_hidden()
                    .px(if self.sidebar_collapsed {
                        px(0.0)
                    } else {
                        px(12.0)
                    })
                    .pb(if self.sidebar_collapsed {
                        px(0.0)
                    } else {
                        px(12.0)
                    })
                    .child(self.render_telemetry_pod(self.sidebar_collapsed, cx)),
            )
            .into_any_element();

        let content_outlet = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .bg(tokens.surface)
            .child(ActiveRouteView::into_any_element(&self.active_view));

        let main_scaffold = Scaffold::new("verve-shell-scaffold", content_outlet)
            .rail(sidebar)
            .snackbar(self.snackbar_host.clone());

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(tokens.surface)
            .text_color(tokens.on_surface)
            .child(title_bar)
            .child(div().flex_1().min_h_0().min_w_0().child(main_scaffold))
            .child(self.sheet_host.clone())
            .child(self.confirm_host.clone())
    }
}
