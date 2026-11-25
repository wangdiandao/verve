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
                DomainKind::Runtime | DomainKind::Profiles | DomainKind::Telemetry
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
            "Proxy routing mode",
            [
                ButtonGroupItem::new("rule", "Rule"),
                ButtonGroupItem::new("global", "Global"),
                ButtonGroupItem::new("direct", "Direct"),
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
            "System Proxy",
            runtime.system_proxy_enabled,
        )
        .supporting(format!(
            "Routes OS HTTP/SOCKS traffic via 127.0.0.1:{}",
            runtime.mixed_port
        ))
        .on_change(move |enabled, _, _| {
            sysproxy_commands.set_system_proxy(enabled);
        });

        let tun_commands = self.commands.clone();
        let tun_subtitle = if settings.local.service_mode_enabled {
            "Privileged service mode enabled for TUN capture"
        } else {
            "Direct unprivileged mode (enable service mode in local config for elevated TUN)"
        };
        let tun_switch = SwitchTile::new("overview-tun-mode", "TUN Mode", runtime.tun_enabled)
            .supporting(tun_subtitle)
            .on_change(move |enabled, _, _| {
                tun_commands.set_tun_enabled(enabled);
            });

        let kernel_commands = self.commands.clone();
        let kernel_running = runtime.kernel_running;
        let kernel_btn = if kernel_running {
            Button::new("overview-kernel-toggle", "Stop Kernel")
                .variant(ButtonVariant::Outlined)
                .leading_icon(symbol_icon(Symbol::STOP))
                .on_activate(move |_, _| kernel_commands.stop_kernel())
        } else {
            Button::new("overview-kernel-toggle", "Start Kernel")
                .variant(ButtonVariant::Filled)
                .leading_icon(symbol_icon(Symbol::PLAY_ARROW))
                .on_activate(move |_, _| kernel_commands.start_kernel())
        };

        let health_state = if runtime.kernel_running {
            KernelHealthState::Running
        } else if runtime.last_error.is_some() {
            KernelHealthState::Fault
        } else {
            KernelHealthState::Stopped
        };

        let network_card = SectionCard::new()
            .title("Network & Mode")
            .subtitle("Routing strategy, OS proxy takeover, and kernel lifecycle")
            .header_trailing(HealthIndicator::new(health_state))
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
                || "No remote quota recorded".to_owned(),
                |u| {
                    format!(
                        "Down {} · Up {}",
                        format_bytes(u.download_bytes),
                        format_bytes(u.upload_bytes)
                    )
                },
            );

            let shell = self.shell.clone();
            SectionCard::new()
                .title("Active Profile")
                .subtitle(usage_text)
                .header_trailing(
                    Button::new("overview-open-profiles", "Manage")
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
                        .child(MetricTile::new("Profile", name))
                        .child(MetricTile::new(
                            "Mixed Port",
                            runtime.mixed_port.to_string(),
                        ))
                        .child(MetricTile::new(
                            "Registered Profiles",
                            profiles.profiles.len().to_string(),
                        )),
                )
        } else {
            let shell = self.shell.clone();
            SectionCard::new().title("Active Profile").child(
                EmptyState::new(
                    "No active profile",
                    "Select or import a profile to compile runtime configuration.",
                )
                .action(
                    Button::new("overview-go-profiles", "Open Profiles")
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
            "Traffic chart window",
            [
                ButtonGroupItem::new("1m", "1m"),
                ButtonGroupItem::new("5m", "5m"),
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
            .title("Live Traffic")
            .subtitle("Real-time kernel throughput and memory telemetry")
            .header_trailing(window_switcher)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(12.0))
                    .child(MetricTile::new(
                        "Download",
                        format_throughput_bps(telemetry.download_bps),
                    ))
                    .child(MetricTile::new(
                        "Upload",
                        format_throughput_bps(telemetry.upload_bps),
                    ))
                    .child(MetricTile::new(
                        "Total Down",
                        format_bytes(telemetry.total_download_bytes),
                    ))
                    .child(MetricTile::new(
                        "Total Up",
                        format_bytes(telemetry.total_upload_bytes),
                    ))
                    .child(MetricTile::new(
                        "Kernel Memory",
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
                            .child("Overview"),
                    )
                    .when_some(runtime.kernel_pid, |row, pid| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(tokens.on_surface_variant)
                                .child(format!("PID {pid}")),
                        )
                    }),
            )
            .child(network_card)
            .child(profile_card)
            .child(traffic_card)
    }
}
