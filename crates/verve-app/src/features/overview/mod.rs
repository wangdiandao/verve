//! Overview feature slice: routing mode, system/TUN takeover, active profile summary,
//! and GPU Canvas live traffic chart.

use gpui::{
    Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, Render,
    ScrollHandle, StatefulInteractiveElement, Styled, Subscription, Window, div,
    prelude::FluentBuilder, px,
};
use gpui_material::{
    Button, ButtonGroup, ButtonGroupItem, ButtonGroupSelection, ButtonGroupVariant, ButtonVariant,
    SwitchTile,
};
use gpui_material_symbols::Symbol;
use verve_core::DomainKind;
use verve_core::kernel::{ProxyMode, TelemetryKind};
use verve_ui_kit::{
    ChartWindow, EmptyState, HealthIndicator, KernelHealthState, MetricTile, SectionCard,
    TrafficChart, TrafficDataPoint, format_bytes, format_throughput_bps, theme_tokens,
};

use crate::bridge::{CommandDispatcher, DomainChanged, UiStore, UiTelemetryLease};
use crate::shell::nav::{RouteId, symbol_icon};
use crate::shell::sheet_host::ShellHandle;

/// Vertical feature view for the Overview dashboard route.
pub struct OverviewView {
    store: Entity<UiStore>,
    commands: CommandDispatcher,
    shell: ShellHandle,
    chart_window: ChartWindow,
    scroll: ScrollHandle,
    _telemetry_lease: UiTelemetryLease,
    _store_sub: Subscription,
}

impl OverviewView {
    /// Creates a new [`OverviewView`], acquiring a scoped `Traffic` [`UiTelemetryLease`].
    pub fn new(
        store: Entity<UiStore>,
        commands: CommandDispatcher,
        shell: ShellHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        let telemetry_lease = store.read(cx).acquire_telemetry(TelemetryKind::Traffic);
        let store_sub = cx.subscribe(&store, |_, _, ev: &DomainChanged, cx| {
            if matches!(
                ev.0,
                DomainKind::Runtime
                    | DomainKind::Profiles
                    | DomainKind::Telemetry
                    | DomainKind::Settings
            ) {
                cx.notify();
            }
        });

        Self {
            store,
            commands,
            shell,
            chart_window: ChartWindow::OneMinute,
            scroll: ScrollHandle::new(),
            _telemetry_lease: telemetry_lease,
            _store_sub: store_sub,
        }
    }
}

impl Render for OverviewView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let store = self.store.read(cx);
        let runtime = store.runtime().clone();
        let profiles = store.profiles().clone();
        let telemetry = store.telemetry().clone();
        let settings = store.settings().clone();

        let selected_mode = match runtime.proxy_mode {
            ProxyMode::Rule => "rule",
            ProxyMode::Global => "global",
            ProxyMode::Direct => "direct",
        };

        let mode_commands = self.commands.clone();
        let mode_group = ButtonGroup::connected(
            "overview-proxy-mode",
            rust_i18n::t!("common.proxy_mode_label").into_owned(),
            [
                ButtonGroupItem::new("rule", rust_i18n::t!("mode.rule").into_owned()),
                ButtonGroupItem::new("global", rust_i18n::t!("mode.global").into_owned()),
                ButtonGroupItem::new("direct", rust_i18n::t!("mode.direct").into_owned()),
            ],
        )
        .variant(ButtonGroupVariant::Connected)
        .selection(ButtonGroupSelection::SingleRequired, [selected_mode.into()])
        .on_change(move |selected, _, _| {
            if let Some(first) = selected.first() {
                let mode = match first.as_ref() {
                    "global" => ProxyMode::Global,
                    "direct" => ProxyMode::Direct,
                    _ => ProxyMode::Rule,
                };
                mode_commands.set_proxy_mode(mode);
            }
        });

        let sysproxy_commands = self.commands.clone();
        let sysproxy_switch = SwitchTile::new(
            "overview-system-proxy",
            rust_i18n::t!("overview.system_proxy.title").into_owned(),
            runtime.system_proxy_enabled,
        )
        .supporting(
            rust_i18n::t!(
                "overview.system_proxy.supporting",
                port = runtime.mixed_port
            )
            .into_owned(),
        )
        .on_change(move |enabled, _, _| {
            sysproxy_commands.set_system_proxy(enabled);
        });

        let tun_commands = self.commands.clone();
        let tun_subtitle = if settings.local.service_mode_enabled {
            rust_i18n::t!("overview.tun.supporting_service").into_owned()
        } else {
            rust_i18n::t!("overview.tun.supporting_direct").into_owned()
        };
        let tun_switch = SwitchTile::new(
            "overview-tun-mode",
            rust_i18n::t!("overview.tun.title").into_owned(),
            runtime.tun_enabled,
        )
        .supporting(tun_subtitle)
        .on_change(move |enabled, _, _| {
            tun_commands.set_tun_enabled(enabled);
        });

        let kernel_commands = self.commands.clone();
        let kernel_running = runtime.kernel_running;
        let kernel_btn = if kernel_running {
            Button::new(
                "overview-kernel-toggle",
                rust_i18n::t!("overview.stop_kernel").into_owned(),
            )
            .variant(ButtonVariant::Outlined)
            .leading_icon(symbol_icon(Symbol::STOP))
            .on_activate(move |_, _| kernel_commands.stop_kernel())
        } else {
            Button::new(
                "overview-kernel-toggle",
                rust_i18n::t!("overview.start_kernel").into_owned(),
            )
            .variant(ButtonVariant::Filled)
            .leading_icon(symbol_icon(Symbol::PLAY_ARROW))
            .on_activate(move |_, _| kernel_commands.start_kernel())
        };

        let (health_state, health_label) = if runtime.kernel_running {
            (
                KernelHealthState::Running,
                rust_i18n::t!("shell.health.running_short").into_owned(),
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

        let network_card = SectionCard::new()
            .title(rust_i18n::t!("overview.network_card.title").into_owned())
            .subtitle(rust_i18n::t!("overview.network_card.subtitle").into_owned())
            .header_trailing(HealthIndicator::new(health_state).label(health_label))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .child(mode_group)
                    .child(kernel_btn),
            )
            .child(sysproxy_switch)
            .child(tun_switch);

        let profile_card = if let Some(active_id) = runtime.active_profile_id.as_ref() {
            let name = runtime
                .active_profile_name
                .clone()
                .unwrap_or_else(|| active_id.clone());
            let usage = profiles.usages.get(active_id);
            let usage_text = usage.map_or_else(
                || rust_i18n::t!("overview.profile_card.no_quota").into_owned(),
                |u| {
                    rust_i18n::t!(
                        "overview.profile_card.usage",
                        down = format_bytes(u.download_bytes),
                        up = format_bytes(u.upload_bytes)
                    )
                    .into_owned()
                },
            );

            let shell = self.shell.clone();
            SectionCard::new()
                .title(rust_i18n::t!("overview.profile_card.title").into_owned())
                .subtitle(usage_text)
                .header_trailing(
                    Button::new(
                        "overview-open-profiles",
                        rust_i18n::t!("overview.profile_card.manage").into_owned(),
                    )
                    .variant(ButtonVariant::Text)
                    .on_activate(move |window, cx| {
                        shell.navigate(RouteId::Profiles, window, cx);
                    }),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap(px(12.0))
                        .child(MetricTile::new(
                            rust_i18n::t!("overview.profile_card.metric_profile").into_owned(),
                            name,
                        ))
                        .child(MetricTile::new(
                            rust_i18n::t!("common.mixed_port").into_owned(),
                            runtime.mixed_port.to_string(),
                        ))
                        .child(MetricTile::new(
                            rust_i18n::t!("overview.profile_card.metric_count").into_owned(),
                            profiles.profiles.len().to_string(),
                        )),
                )
        } else {
            let shell = self.shell.clone();
            SectionCard::new()
                .title(rust_i18n::t!("overview.profile_card.title").into_owned())
                .child(
                    EmptyState::new(
                        rust_i18n::t!("overview.profile_card.empty_title").into_owned(),
                        rust_i18n::t!("overview.profile_card.empty_desc").into_owned(),
                    )
                    .action(
                        Button::new(
                            "overview-go-profiles",
                            rust_i18n::t!("overview.profile_card.open_profiles").into_owned(),
                        )
                        .variant(ButtonVariant::FilledTonal)
                        .on_activate(move |window, cx| {
                            shell.navigate(RouteId::Profiles, window, cx);
                        }),
                    ),
                )
        };

        let chart_points: Vec<TrafficDataPoint> = telemetry
            .traffic_history
            .iter()
            .map(|s| TrafficDataPoint {
                up_bps: s.up_bps,
                down_bps: s.down_bps,
            })
            .collect();

        let view = cx.weak_entity();
        let selected_window = match self.chart_window {
            ChartWindow::OneMinute => "1m",
            ChartWindow::FiveMinutes => "5m",
        };
        let window_switcher = ButtonGroup::connected(
            "overview-chart-window",
            rust_i18n::t!("overview.traffic_card.window_label").into_owned(),
            [
                ButtonGroupItem::new(
                    "1m",
                    rust_i18n::t!("overview.traffic_card.window_1m").into_owned(),
                ),
                ButtonGroupItem::new(
                    "5m",
                    rust_i18n::t!("overview.traffic_card.window_5m").into_owned(),
                ),
            ],
        )
        .selection(
            ButtonGroupSelection::SingleRequired,
            [selected_window.into()],
        )
        .on_change(move |selected, _, cx| {
            if let Some(first) = selected.first() {
                let next = match first.as_ref() {
                    "5m" => ChartWindow::FiveMinutes,
                    _ => ChartWindow::OneMinute,
                };
                let _ = view.update(cx, |this, cx| {
                    this.chart_window = next;
                    cx.notify();
                });
            }
        });

        let traffic_card = SectionCard::new()
            .title(rust_i18n::t!("overview.traffic_card.title").into_owned())
            .subtitle(rust_i18n::t!("overview.traffic_card.subtitle").into_owned())
            .header_trailing(window_switcher)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(12.0))
                    .child(MetricTile::new(
                        rust_i18n::t!("overview.traffic_card.download").into_owned(),
                        format_throughput_bps(telemetry.download_bps),
                    ))
                    .child(MetricTile::new(
                        rust_i18n::t!("overview.traffic_card.upload").into_owned(),
                        format_throughput_bps(telemetry.upload_bps),
                    ))
                    .child(MetricTile::new(
                        rust_i18n::t!("overview.traffic_card.total_down").into_owned(),
                        format_bytes(telemetry.total_download_bytes),
                    ))
                    .child(MetricTile::new(
                        rust_i18n::t!("overview.traffic_card.total_up").into_owned(),
                        format_bytes(telemetry.total_upload_bytes),
                    ))
                    .child(MetricTile::new(
                        rust_i18n::t!("overview.traffic_card.memory").into_owned(),
                        format_bytes(telemetry.memory_bytes),
                    )),
            )
            .child(TrafficChart::new(chart_points).window(self.chart_window));

        div()
            .id("overview-page")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p(px(20.0))
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(tokens.on_surface)
                            .child(rust_i18n::t!("overview.title").into_owned()),
                    )
                    .when_some(runtime.kernel_pid, |row, pid| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(tokens.on_surface_variant)
                                .child(rust_i18n::t!("overview.pid", pid = pid).into_owned()),
                        )
                    }),
            )
            .child(network_card)
            .child(profile_card)
            .child(traffic_card)
    }
}
