//! Overview feature slice: responsive two-column master control & active profile/outbound
//! quick-switch cards, terminal proxy command bar, GPU Canvas live traffic chart, and
//! compact runtime status bar.

pub mod terminal_proxy;

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{
    AppContext, ClipboardItem, Context, ElementId, Entity, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Render, ScrollHandle, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Window, div, prelude::FluentBuilder, px,
};
use gpui_material::{
    Button, ButtonGroup, ButtonGroupItem, ButtonGroupSelection, ButtonGroupVariant, ButtonVariant,
    Chip, Divider, DropdownAnchor, DropdownOption, ExposedDropdown, ExposedDropdownState,
    IconButton, LinearProgress, SemanticTone, SwitchTile,
};
use gpui_material_symbols::Symbol;
use verve_core::config::{DEFAULT_DELAY_TEST_URL, ProfileSource, ProfileUsage};
use verve_core::kernel::{ProxyGroup, ProxyGroupKind, ProxyMode, TelemetryKind};
use verve_core::{
    DomainKind, ProfilesSlice, ProxiesSlice, RuntimeSlice, ServiceStatus, SettingsSlice,
    TelemetrySlice,
};
use verve_ui_kit::{
    ChartWindow, EmptyState, HealthIndicator, KernelHealthState, LatencyBadge, MetricTile,
    SectionCard, TrafficChart, TrafficDataPoint, format_bytes, format_duration_hms,
    format_throughput_bps, format_unix_date, theme_tokens,
};

pub use terminal_proxy::{TerminalShell, format_terminal_proxy_command};

use crate::bridge::{CommandDispatcher, DomainChanged, UiStore, UiTelemetryLease};
use crate::shell::nav::{RouteId, symbol_icon};
use crate::shell::sheet_host::ShellHandle;

/// Viewport width threshold (in logical pixels) at which Overview switches to a two-column layout.
pub const TWO_COLUMN_BREAKPOINT_PX: f32 = 920.0;

const LOOPBACK_PROXY_HOST: &str = "127.0.0.1";
const DEFAULT_DELAY_TIMEOUT: Duration = Duration::from_secs(5);

/// Computed traffic quota summary for an active subscription profile.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuotaSummary {
    /// Total consumed traffic (`upload_bytes + download_bytes`, saturating).
    pub used_bytes: u64,
    /// Optional total quota reported by the subscription provider.
    pub total_bytes: Option<u64>,
    /// Clamped progress fraction (`0.0..=1.0`) when `total_bytes > 0`.
    pub progress: Option<f32>,
    /// Rounded whole percentage (`0..=100`) when `total_bytes > 0`.
    pub percent: Option<u8>,
}

/// Resolved expiration state of a subscription profile relative to the current timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileExpiryStatus {
    /// Subscription has no expiration timestamp (or non-positive sentinel).
    Never,
    /// Subscription expires on the formatted UTC date (`YYYY-MM-DD`).
    Valid(String),
    /// Subscription already expired on the formatted UTC date (`YYYY-MM-DD`).
    Expired(String),
}

/// Computes used bytes, clamped progress fraction (`0.0..=1.0`), and whole percentage from [`ProfileUsage`].
#[must_use]
pub fn compute_quota_summary(usage: &ProfileUsage) -> QuotaSummary {
    let used_bytes = usage.upload_bytes.saturating_add(usage.download_bytes);
    match usage.total_bytes {
        Some(total) if total > 0 => {
            let ratio = (used_bytes as f64 / total as f64).clamp(0.0, 1.0);
            let progress = ratio as f32;
            let percent = (ratio * 100.0).round().clamp(0.0, 100.0) as u8;
            QuotaSummary {
                used_bytes,
                total_bytes: Some(total),
                progress: Some(progress),
                percent: Some(percent),
            }
        }
        total_bytes => QuotaSummary {
            used_bytes,
            total_bytes,
            progress: None,
            percent: None,
        },
    }
}

/// Resolves a subscription expiration timestamp against `now_unix_secs` into a displayable status.
#[must_use]
pub fn resolve_profile_expiry(
    expire_unix_secs: Option<i64>,
    now_unix_secs: i64,
) -> ProfileExpiryStatus {
    match expire_unix_secs {
        None => ProfileExpiryStatus::Never,
        Some(ts) if ts <= 0 => ProfileExpiryStatus::Never,
        Some(ts) if ts <= now_unix_secs => ProfileExpiryStatus::Expired(format_unix_date(ts)),
        Some(ts) => ProfileExpiryStatus::Valid(format_unix_date(ts)),
    }
}

/// Selects the primary visible proxy group for Overview quick-switching.
///
/// Prefers the first non-hidden [`ProxyGroupKind::Select`] group; falls back to the first
/// non-hidden group of any kind when no selector group is present.
#[must_use]
pub fn select_primary_proxy_group(groups: &[ProxyGroup]) -> Option<&ProxyGroup> {
    groups
        .iter()
        .find(|g| !g.hidden && g.kind == ProxyGroupKind::Select)
        .or_else(|| groups.iter().find(|g| !g.hidden))
}

/// Formats kernel uptime via [`format_duration_hms`], or `"--"` when the kernel is stopped.
#[must_use]
pub fn format_kernel_uptime(
    kernel_running: bool,
    started_at_unix_secs: Option<i64>,
    now_unix_secs: i64,
) -> String {
    if !kernel_running {
        return "--".to_owned();
    }
    match started_at_unix_secs {
        Some(started) => {
            let elapsed = now_unix_secs.saturating_sub(started).max(0) as u64;
            format_duration_hms(elapsed)
        }
        None => "--".to_owned(),
    }
}

/// Returns `true` when the Overview grid should render in its two-column desktop layout.
#[must_use]
pub fn uses_two_column_layout(viewport_width_px: f32) -> bool {
    viewport_width_px >= TWO_COLUMN_BREAKPOINT_PX
}

fn current_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

struct OverviewSnapshot<'a> {
    runtime: &'a RuntimeSlice,
    profiles: &'a ProfilesSlice,
    proxies: &'a ProxiesSlice,
    telemetry: &'a TelemetrySlice,
    settings: &'a SettingsSlice,
    now_unix_secs: i64,
}

/// Vertical feature view for the Overview dashboard route.
pub struct OverviewView {
    store: Entity<UiStore>,
    commands: CommandDispatcher,
    shell: ShellHandle,
    chart_window: ChartWindow,
    terminal_shell: TerminalShell,
    scroll: ScrollHandle,
    profile_dropdown: Option<Entity<ExposedDropdownState>>,
    last_profile_display: Option<String>,
    outbound_dropdown: Option<Entity<ExposedDropdownState>>,
    last_outbound_display: Option<String>,
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
                    | DomainKind::Proxies
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
            terminal_shell: TerminalShell::default_for_platform(),
            scroll: ScrollHandle::new(),
            profile_dropdown: None,
            last_profile_display: None,
            outbound_dropdown: None,
            last_outbound_display: None,
            _telemetry_lease: telemetry_lease,
            _store_sub: store_sub,
        }
    }

    /// Returns the currently selected shell for the terminal proxy command quick bar.
    #[must_use]
    pub const fn terminal_shell(&self) -> TerminalShell {
        self.terminal_shell
    }

    /// Switches the selected terminal shell and notifies GPUI to re-render.
    pub fn set_terminal_shell(&mut self, shell: TerminalShell, cx: &mut Context<Self>) {
        if self.terminal_shell != shell {
            self.terminal_shell = shell;
            cx.notify();
        }
    }

    /// Returns the currently selected traffic chart time window.
    #[must_use]
    pub const fn chart_window(&self) -> ChartWindow {
        self.chart_window
    }

    fn ensure_profile_dropdown(
        &mut self,
        current_label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ExposedDropdownState> {
        if let Some(state) = &self.profile_dropdown {
            if self.last_profile_display.as_deref() != Some(current_label) {
                let next_label = current_label.to_owned();
                state.update(cx, |dropdown, cx| {
                    dropdown.set_value(next_label.clone(), window, cx);
                });
                self.last_profile_display = Some(next_label);
            }
            return state.clone();
        }

        let initial = current_label.to_owned();
        let state = cx.new(|cx| ExposedDropdownState::new(initial.clone(), false, window, cx));
        self.profile_dropdown = Some(state.clone());
        self.last_profile_display = Some(initial);
        state
    }

    fn ensure_outbound_dropdown(
        &mut self,
        current_label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ExposedDropdownState> {
        if let Some(state) = &self.outbound_dropdown {
            if self.last_outbound_display.as_deref() != Some(current_label) {
                let next_label = current_label.to_owned();
                state.update(cx, |dropdown, cx| {
                    dropdown.set_value(next_label.clone(), window, cx);
                });
                self.last_outbound_display = Some(next_label);
            }
            return state.clone();
        }

        let initial = current_label.to_owned();
        let state = cx.new(|cx| ExposedDropdownState::new(initial.clone(), false, window, cx));
        self.outbound_dropdown = Some(state.clone());
        self.last_outbound_display = Some(initial);
        state
    }

    fn render_network_card(
        &self,
        runtime: &RuntimeSlice,
        settings: &SettingsSlice,
        cx: &mut Context<Self>,
    ) -> SectionCard {
        let tokens = theme_tokens(cx);
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

        let restart_commands = self.commands.clone();
        let restart_btn = Button::new(
            "overview-kernel-restart",
            rust_i18n::t!("overview.restart_kernel").into_owned(),
        )
        .variant(ButtonVariant::FilledTonal)
        .leading_icon(symbol_icon(Symbol::RESTART_ALT))
        .disabled(!kernel_running)
        .on_activate(move |_, _| restart_commands.restart_kernel());

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
            match &runtime.service_status {
                ServiceStatus::Available { .. } => {
                    rust_i18n::t!("overview.tun.supporting_service_available").into_owned()
                }
                ServiceStatus::Unavailable { .. } => {
                    rust_i18n::t!("overview.tun.supporting_service_unavailable").into_owned()
                }
                ServiceStatus::Unknown => {
                    rust_i18n::t!("overview.tun.supporting_service").into_owned()
                }
            }
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

        let active_shell = self.terminal_shell;
        let terminal_cmd =
            format_terminal_proxy_command(active_shell, LOOPBACK_PROXY_HOST, runtime.mixed_port);
        let copy_cmd = terminal_cmd.clone();
        let copy_commands = self.commands.clone();
        let copy_button = Button::new(
            "overview-terminal-proxy-copy",
            rust_i18n::t!("overview.terminal_proxy.copy").into_owned(),
        )
        .variant(ButtonVariant::FilledTonal)
        .leading_icon(symbol_icon(Symbol::CONTENT_COPY))
        .on_activate(move |_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy_cmd.clone()));
            copy_commands.notify_info(
                rust_i18n::t!(
                    "overview.terminal_proxy.copied",
                    shell = active_shell.label()
                )
                .into_owned(),
            );
        });

        let mut shell_chips = div().flex().flex_row().flex_wrap().gap(px(6.0));
        for (idx, shell) in TerminalShell::ALL.into_iter().enumerate() {
            let view = cx.weak_entity();
            shell_chips = shell_chips.child(
                Chip::filter(("overview-terminal-shell", idx), shell.label())
                    .selected(active_shell == shell)
                    .on_change(move |_, _, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.set_terminal_shell(shell, cx);
                        });
                    }),
            );
        }

        let terminal_bar = div()
            .w_full()
            .p(px(12.0))
            .rounded(px(12.0))
            .bg(tokens.surface_container)
            .border_1()
            .border_color(tokens.outline_variant)
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(tokens.on_surface)
                            .child(symbol_icon(Symbol::TERMINAL))
                            .child(rust_i18n::t!("overview.terminal_proxy.title").into_owned()),
                    )
                    .child(copy_button),
            )
            .child(shell_chips)
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .px(px(10.0))
                    .py(px(8.0))
                    .rounded(px(8.0))
                    .bg(tokens.surface_container_lowest)
                    .border_1()
                    .border_color(tokens.outline_variant)
                    .text_xs()
                    .text_color(tokens.on_surface_variant)
                    .overflow_hidden()
                    .truncate()
                    .child(terminal_cmd),
            );

        SectionCard::new()
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
                    .gap(px(10.0))
                    .child(mode_group)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .items_center()
                            .gap(px(8.0))
                            .child(kernel_btn)
                            .child(restart_btn),
                    ),
            )
            .child(sysproxy_switch)
            .child(tun_switch)
            .child(Divider::horizontal())
            .child(terminal_bar)
    }

    fn render_profile_outbound_card(
        &mut self,
        snapshot: &OverviewSnapshot<'_>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> SectionCard {
        let runtime = snapshot.runtime;
        let profiles = snapshot.profiles;
        let proxies = snapshot.proxies;
        let settings = snapshot.settings;
        let now_unix_secs = snapshot.now_unix_secs;
        let tokens = theme_tokens(cx);
        let manage_shell = self.shell.clone();
        let active_id = runtime
            .active_profile_id
            .as_ref()
            .or(profiles.active_profile_id.as_ref());
        let active_entry =
            active_id.and_then(|id| profiles.profiles.iter().find(|entry| &entry.id == id));

        let mut card = SectionCard::new()
            .title(rust_i18n::t!("overview.profile_card.title").into_owned())
            .header_trailing(
                Button::new(
                    "overview-open-profiles",
                    rust_i18n::t!("overview.profile_card.manage").into_owned(),
                )
                .variant(ButtonVariant::Text)
                .on_activate(move |window, cx| {
                    manage_shell.navigate(RouteId::Profiles, window, cx);
                }),
            );

        if profiles.profiles.is_empty() {
            let empty_shell = self.shell.clone();
            card = card.child(
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
                        empty_shell.navigate(RouteId::Profiles, window, cx);
                    }),
                ),
            );
        } else {
            let current_profile_label = active_entry
                .map(|entry| entry.name.clone())
                .or_else(|| runtime.active_profile_name.clone())
                .unwrap_or_else(|| {
                    rust_i18n::t!("overview.profile_card.select_placeholder").into_owned()
                });

            let mut profile_id_map = HashMap::with_capacity(profiles.profiles.len());
            let mut profile_options = Vec::with_capacity(profiles.profiles.len());
            for entry in &profiles.profiles {
                let opt_id = ElementId::Name(SharedString::from(format!(
                    "overview-profile-opt-{}",
                    entry.id
                )));
                profile_id_map.insert(opt_id.clone(), entry.id.clone());
                profile_options.push(DropdownOption::new(opt_id, entry.name.clone()));
            }
            let profile_id_map = Rc::new(profile_id_map);
            let dropdown_state = self.ensure_profile_dropdown(&current_profile_label, window, cx);
            let activate_commands = self.commands.clone();
            let profile_dropdown = ExposedDropdown::new(
                "overview-profile-switcher",
                rust_i18n::t!("overview.profile_card.selector_label").into_owned(),
                &dropdown_state,
                profile_options,
            )
            .primary_anchor(DropdownAnchor::PrimaryNotEditable)
            .on_select(move |option, _, _| {
                if let Some(id) = profile_id_map.get(&option.id) {
                    activate_commands.activate_profile(id.clone());
                }
            });

            let remote_refresh_btn = active_entry.and_then(|entry| {
                matches!(entry.source, ProfileSource::Remote { .. }).then(|| {
                    let refresh_commands = self.commands.clone();
                    let refresh_id = entry.id.clone();
                    IconButton::tonal(
                        "overview-profile-refresh",
                        rust_i18n::t!("overview.profile_card.refresh_tooltip").into_owned(),
                        symbol_icon(Symbol::REFRESH),
                    )
                    .on_activate(move |_, _| {
                        refresh_commands.refresh_profile(refresh_id.clone());
                    })
                })
            });

            card = card.child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().flex_1().min_w_0().child(profile_dropdown))
                    .when_some(remote_refresh_btn, |row, btn| row.child(btn)),
            );

            if let Some(active_profile_id) = active_id {
                if let Some(usage) = profiles.usages.get(active_profile_id) {
                    let quota = compute_quota_summary(usage);
                    let expiry = resolve_profile_expiry(usage.expire_unix_secs, now_unix_secs);

                    let quota_label = match (quota.total_bytes, quota.percent) {
                        (Some(total), Some(percent)) if total > 0 => rust_i18n::t!(
                            "overview.profile_card.quota_used_total",
                            used = format_bytes(quota.used_bytes),
                            total = format_bytes(total),
                            percent = percent
                        )
                        .into_owned(),
                        _ => rust_i18n::t!(
                            "overview.profile_card.usage",
                            down = format_bytes(usage.download_bytes),
                            up = format_bytes(usage.upload_bytes)
                        )
                        .into_owned(),
                    };

                    let (expiry_label, expiry_color) = match expiry {
                        ProfileExpiryStatus::Never => (
                            rust_i18n::t!("common.never").into_owned(),
                            tokens.on_surface_variant,
                        ),
                        ProfileExpiryStatus::Valid(date) => (
                            rust_i18n::t!("overview.profile_card.expires_on", date = date)
                                .into_owned(),
                            tokens.on_surface_variant,
                        ),
                        ProfileExpiryStatus::Expired(date) => (
                            format!("{} · {date}", rust_i18n::t!("common.expired")),
                            tokens.error,
                        ),
                    };

                    let progress_tone = match quota.progress {
                        Some(p) if p >= 0.9 => SemanticTone::Danger,
                        Some(p) if p >= 0.75 => SemanticTone::Warning,
                        _ => SemanticTone::Default,
                    };

                    let usage_breakdown = rust_i18n::t!(
                        "overview.profile_card.usage",
                        down = format_bytes(usage.download_bytes),
                        up = format_bytes(usage.upload_bytes)
                    )
                    .into_owned();

                    card = card.child(
                        div()
                            .w_full()
                            .p(px(12.0))
                            .rounded(px(12.0))
                            .bg(tokens.surface_container)
                            .border_1()
                            .border_color(tokens.outline_variant)
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .flex_wrap()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(tokens.on_surface)
                                            .child(quota_label),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(expiry_color)
                                            .child(expiry_label),
                                    ),
                            )
                            .when_some(quota.progress, |block, progress| {
                                block.child(
                                    LinearProgress::new(
                                        "overview-profile-quota-progress",
                                        rust_i18n::t!("overview.profile_card.quota_progress_label")
                                            .into_owned(),
                                    )
                                    .value(progress)
                                    .tone(progress_tone),
                                )
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(tokens.on_surface_variant)
                                    .child(usage_breakdown),
                            ),
                    );
                } else {
                    card = card.child(
                        div()
                            .text_xs()
                            .text_color(tokens.on_surface_variant)
                            .child(rust_i18n::t!("overview.profile_card.no_quota").into_owned()),
                    );
                }
            } else {
                card = card.child(
                    div()
                        .text_xs()
                        .text_color(tokens.on_surface_variant)
                        .child(rust_i18n::t!("overview.profile_card.select_hint").into_owned()),
                );
            }
        }

        // Primary Proxy Group Outbound Quick-Switch section.
        let proxies_shell = self.shell.clone();
        let primary_group = select_primary_proxy_group(&proxies.groups);

        let outbound_header = div()
            .w_full()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .min_w_0()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(tokens.on_surface)
                            .child(rust_i18n::t!("overview.outbound.title").into_owned()),
                    )
                    .when_some(primary_group, |row, group| {
                        row.child(
                            div()
                                .px(px(8.0))
                                .py(px(2.0))
                                .rounded_full()
                                .bg(tokens.secondary_container)
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(tokens.on_secondary_container)
                                .child(group.name.clone()),
                        )
                    }),
            )
            .child(
                Button::new(
                    "overview-open-proxies",
                    rust_i18n::t!("overview.outbound.all_proxies").into_owned(),
                )
                .variant(ButtonVariant::Text)
                .on_activate(move |window, cx| {
                    proxies_shell.navigate(RouteId::Proxies, window, cx);
                }),
            );

        let outbound_body = if runtime.proxy_mode == ProxyMode::Direct {
            div()
                .w_full()
                .p(px(12.0))
                .rounded(px(12.0))
                .bg(tokens.surface_container)
                .border_1()
                .border_color(tokens.outline_variant)
                .text_xs()
                .text_color(tokens.on_surface_variant)
                .child(rust_i18n::t!("overview.outbound.direct_mode").into_owned())
        } else if let Some(group) = primary_group {
            let group_name = group.name.clone();
            let current_now = group.now.clone();
            let current_display = current_now.clone().unwrap_or_else(|| "--".to_owned());
            let current_delay = current_now
                .as_ref()
                .and_then(|node| proxies.nodes.get(node))
                .and_then(|meta| meta.latest_delay_ms);

            let delay_url = if settings.syncable.default_delay_test_url.trim().is_empty() {
                DEFAULT_DELAY_TEST_URL.to_owned()
            } else {
                settings.syncable.default_delay_test_url.clone()
            };

            let probe_commands = self.commands.clone();
            let probe_target = current_now.clone();
            let probe_tooltip = rust_i18n::t!(
                "overview.outbound.probe_tooltip",
                node = current_display.as_str()
            )
            .into_owned();
            let probe_btn = IconButton::tonal(
                "overview-outbound-probe",
                probe_tooltip,
                symbol_icon(Symbol::SPEED),
            )
            .disabled(probe_target.is_none() || !runtime.kernel_running)
            .on_activate(move |_, _| {
                if let Some(node) = probe_target.as_ref() {
                    probe_commands.test_delay(
                        node.clone(),
                        delay_url.clone(),
                        DEFAULT_DELAY_TIMEOUT,
                    );
                }
            });

            let mut node_name_map = HashMap::with_capacity(group.all.len());
            let mut node_delay_map = HashMap::with_capacity(group.all.len());
            let mut node_options = Vec::with_capacity(group.all.len());
            for (idx, member) in group.all.iter().enumerate() {
                let opt_id =
                    ElementId::Name(SharedString::from(format!("overview-outbound-opt-{idx}")));
                let delay = proxies.nodes.get(member).and_then(|n| n.latest_delay_ms);
                node_name_map.insert(opt_id.clone(), member.clone());
                node_delay_map.insert(opt_id.clone(), delay);
                node_options.push(DropdownOption::new(opt_id, member.clone()));
            }
            let node_name_map = Rc::new(node_name_map);
            let node_delay_map = Rc::new(node_delay_map);

            let outbound_dropdown_state =
                self.ensure_outbound_dropdown(&current_display, window, cx);
            let select_commands = self.commands.clone();
            let select_group = group_name;
            let can_select = runtime.kernel_running
                && group.kind == ProxyGroupKind::Select
                && !group.all.is_empty();

            let outbound_dropdown = ExposedDropdown::new(
                "overview-outbound-switcher",
                rust_i18n::t!("overview.outbound.selector_label").into_owned(),
                &outbound_dropdown_state,
                node_options,
            )
            .primary_anchor(DropdownAnchor::PrimaryNotEditable)
            .disabled(!can_select)
            .option_content(move |option, _, _, _| {
                let delay = node_delay_map.get(&option.id).copied().flatten();
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(option.label.clone()),
                    )
                    .child(LatencyBadge::new(delay))
            })
            .on_select(move |option, _, _| {
                if let Some(node) = node_name_map.get(&option.id) {
                    select_commands.select_proxy(select_group.clone(), node.clone());
                }
            });

            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(
                    div()
                        .w_full()
                        .px(px(12.0))
                        .py(px(10.0))
                        .rounded(px(12.0))
                        .bg(tokens.surface_container)
                        .border_1()
                        .border_color(tokens.outline_variant)
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap(px(8.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(tokens.on_surface)
                                        .child(current_display),
                                )
                                .child(LatencyBadge::new(current_delay)),
                        )
                        .child(probe_btn),
                )
                .when(!group.all.is_empty(), |col| col.child(outbound_dropdown))
        } else {
            let empty_msg = if runtime.kernel_running {
                rust_i18n::t!("overview.outbound.empty_running").into_owned()
            } else {
                rust_i18n::t!("overview.outbound.empty_stopped").into_owned()
            };
            div()
                .w_full()
                .p(px(12.0))
                .rounded(px(12.0))
                .bg(tokens.surface_container)
                .border_1()
                .border_color(tokens.outline_variant)
                .text_xs()
                .text_color(tokens.on_surface_variant)
                .child(empty_msg)
        };

        card.child(Divider::horizontal())
            .child(outbound_header)
            .child(outbound_body)
    }

    fn render_traffic_card(
        &self,
        telemetry: &TelemetrySlice,
        cx: &mut Context<Self>,
    ) -> SectionCard {
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

        SectionCard::new()
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
            .child(TrafficChart::new(chart_points).window(self.chart_window))
    }

    fn render_status_bar(
        &self,
        runtime: &RuntimeSlice,
        settings: &SettingsSlice,
        now_unix_secs: i64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let kernel_version = runtime
            .kernel_version
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or("--")
            .to_owned();
        let mixed_port = format!(":{}", runtime.mixed_port);
        let uptime = format_kernel_uptime(
            runtime.kernel_running,
            runtime.kernel_started_at_unix_secs,
            now_unix_secs,
        );

        let (service_label, service_dot_color) = if settings.local.service_mode_enabled {
            match &runtime.service_status {
                ServiceStatus::Available { .. } => (
                    rust_i18n::t!("overview.status_bar.service_active").into_owned(),
                    tokens.success,
                ),
                ServiceStatus::Unavailable { .. } => (
                    rust_i18n::t!("overview.status_bar.service_unavailable").into_owned(),
                    tokens.error,
                ),
                ServiceStatus::Unknown => (
                    rust_i18n::t!("overview.status_bar.service_probing").into_owned(),
                    tokens.warning,
                ),
            }
        } else {
            match &runtime.service_status {
                ServiceStatus::Available { .. } => (
                    rust_i18n::t!("overview.status_bar.service_direct_ready").into_owned(),
                    tokens.primary,
                ),
                ServiceStatus::Unavailable { .. } | ServiceStatus::Unknown => (
                    rust_i18n::t!("overview.status_bar.service_direct").into_owned(),
                    tokens.outline,
                ),
            }
        };

        let status_item = |label: String, value: String| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(tokens.on_surface_variant)
                        .child(label),
                )
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(tokens.on_surface)
                        .child(value),
                )
        };

        div()
            .id("overview-status-bar")
            .w_full()
            .px(px(16.0))
            .py(px(10.0))
            .rounded(px(12.0))
            .bg(tokens.surface_container_low)
            .border_1()
            .border_color(tokens.outline_variant)
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .child(status_item(
                rust_i18n::t!("overview.status_bar.kernel").into_owned(),
                kernel_version,
            ))
            .child(status_item(
                rust_i18n::t!("common.mixed_port").into_owned(),
                mixed_port,
            ))
            .child(status_item(
                rust_i18n::t!("overview.status_bar.uptime").into_owned(),
                uptime,
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.on_surface_variant)
                            .child(rust_i18n::t!("overview.status_bar.service").into_owned()),
                    )
                    .child(div().size(px(8.0)).rounded_full().bg(service_dot_color))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(tokens.on_surface)
                            .child(service_label),
                    ),
            )
    }
}

impl Render for OverviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let (runtime, profiles, proxies, telemetry, settings) = {
            let store = self.store.read(cx);
            (
                store.runtime().clone(),
                store.profiles().clone(),
                store.proxies().clone(),
                store.telemetry().clone(),
                store.settings().clone(),
            )
        };

        let now_unix_secs = current_unix_secs();
        let is_wide = uses_two_column_layout(f32::from(window.viewport_size().width));
        let snapshot = OverviewSnapshot {
            runtime: &runtime,
            profiles: &profiles,
            proxies: &proxies,
            telemetry: &telemetry,
            settings: &settings,
            now_unix_secs,
        };

        let network_card = self.render_network_card(snapshot.runtime, snapshot.settings, cx);
        let profile_outbound_card = self.render_profile_outbound_card(&snapshot, window, cx);
        let traffic_card = self.render_traffic_card(snapshot.telemetry, cx);
        let status_bar = self.render_status_bar(
            snapshot.runtime,
            snapshot.settings,
            snapshot.now_unix_secs,
            cx,
        );

        let top_grid = if is_wide {
            div()
                .w_full()
                .flex()
                .flex_row()
                .items_start()
                .gap(px(16.0))
                .child(div().flex_1().min_w_0().child(network_card))
                .child(div().flex_1().min_w_0().child(profile_outbound_card))
        } else {
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(16.0))
                .child(network_card)
                .child(profile_outbound_card)
        };

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
            .child(top_grid)
            .child(traffic_card)
            .child(status_bar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_summary_computes_and_clamps_progress() {
        let normal = ProfileUsage {
            profile_id: "p1".into(),
            upload_bytes: 256,
            download_bytes: 768,
            total_bytes: Some(2_048),
            expire_unix_secs: None,
            updated_at_unix_secs: 0,
        };
        let summary = compute_quota_summary(&normal);
        assert_eq!(summary.used_bytes, 1_024);
        assert_eq!(summary.total_bytes, Some(2_048));
        assert_eq!(summary.progress, Some(0.5));
        assert_eq!(summary.percent, Some(50));

        // Over-quota clamps to 1.0 / 100%.
        let over = ProfileUsage {
            profile_id: "p2".into(),
            upload_bytes: 3_000,
            download_bytes: 2_000,
            total_bytes: Some(4_000),
            expire_unix_secs: None,
            updated_at_unix_secs: 0,
        };
        let over_summary = compute_quota_summary(&over);
        assert_eq!(over_summary.used_bytes, 5_000);
        assert_eq!(over_summary.progress, Some(1.0));
        assert_eq!(over_summary.percent, Some(100));

        // Missing or zero total_bytes yields None progress.
        let unlimited = ProfileUsage {
            profile_id: "p3".into(),
            upload_bytes: 100,
            download_bytes: 200,
            total_bytes: Some(0),
            expire_unix_secs: None,
            updated_at_unix_secs: 0,
        };
        assert_eq!(compute_quota_summary(&unlimited).progress, None);
        assert_eq!(compute_quota_summary(&unlimited).percent, None);
    }

    #[test]
    fn resolves_profile_expiry_never_valid_and_expired() {
        let now = 1_795_680_300; // 2026-11-26
        assert_eq!(
            resolve_profile_expiry(None, now),
            ProfileExpiryStatus::Never
        );
        assert_eq!(
            resolve_profile_expiry(Some(0), now),
            ProfileExpiryStatus::Never
        );
        assert_eq!(
            resolve_profile_expiry(Some(-100), now),
            ProfileExpiryStatus::Never
        );
        assert_eq!(
            resolve_profile_expiry(Some(1_709_251_199), now),
            ProfileExpiryStatus::Expired("2024-02-29".into())
        );
        assert_eq!(
            resolve_profile_expiry(Some(1_800_000_000), now),
            ProfileExpiryStatus::Valid("2027-01-15".into())
        );
    }

    #[test]
    fn selects_primary_proxy_group_preferring_visible_select() {
        let hidden_select = ProxyGroup {
            name: "Hidden".into(),
            kind: ProxyGroupKind::Select,
            now: Some("hk-01".into()),
            all: vec!["hk-01".into()],
            hidden: true,
        };
        let url_test = ProxyGroup {
            name: "Auto".into(),
            kind: ProxyGroupKind::UrlTest,
            now: Some("sg-01".into()),
            all: vec!["sg-01".into()],
            hidden: false,
        };
        let visible_select = ProxyGroup {
            name: "Proxy".into(),
            kind: ProxyGroupKind::Select,
            now: Some("hk-01".into()),
            all: vec!["hk-01".into(), "jp-01".into()],
            hidden: false,
        };

        // Prefers visible Select even when UrlTest comes first.
        let mixed = [
            hidden_select.clone(),
            url_test.clone(),
            visible_select.clone(),
        ];
        let picked = select_primary_proxy_group(&mixed);
        assert_eq!(picked.map(|g| g.name.as_str()), Some("Proxy"));

        // Falls back to first visible non-Select when no visible Select exists.
        let non_select = [hidden_select.clone(), url_test];
        let fallback = select_primary_proxy_group(&non_select);
        assert_eq!(fallback.map(|g| g.name.as_str()), Some("Auto"));

        // Returns None when all groups are hidden or empty.
        let only_hidden = [hidden_select];
        assert!(select_primary_proxy_group(&only_hidden).is_none());
        assert!(select_primary_proxy_group(&[]).is_none());
    }

    #[test]
    fn formats_kernel_uptime_and_responsive_breakpoint() {
        assert_eq!(format_kernel_uptime(false, Some(1_000), 1_252), "--");
        assert_eq!(format_kernel_uptime(true, None, 1_252), "--");
        assert_eq!(format_kernel_uptime(true, Some(1_000), 1_252), "00:04:12");
        assert_eq!(format_kernel_uptime(true, Some(2_000), 1_500), "00:00:00");

        assert!(uses_two_column_layout(1080.0));
        assert!(uses_two_column_layout(920.0));
        assert!(!uses_two_column_layout(919.9));
        assert!(!uses_two_column_layout(760.0));
    }
}
