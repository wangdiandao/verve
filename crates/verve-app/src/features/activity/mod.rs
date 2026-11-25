//! Activity feature slice: scoped Connections / Rules / Logs tabs with on-demand telemetry leases.

use gpui::{
    Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, Render,
    ScrollHandle, StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use gpui_material::{Button, ButtonGroup, ButtonGroupItem, ButtonGroupSelection, ButtonVariant};
use verve_core::DomainKind;
use verve_core::kernel::TelemetryKind;
use verve_ui_kit::{EmptyState, SectionCard, format_bytes, theme_tokens};

use crate::bridge::{CommandDispatcher, DomainChanged, UiStore, UiTelemetryLease};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivityTab {
    Connections,
    Rules,
    Logs,
}

impl ActivityTab {
    const fn id_str(self) -> &'static str {
        match self {
            Self::Connections => "connections",
            Self::Rules => "rules",
            Self::Logs => "logs",
        }
    }
}

/// Vertical feature view holding a [`UiTelemetryLease`] solely for the active tab.
pub struct ActivityView {
    store: Entity<UiStore>,
    commands: CommandDispatcher,
    active_tab: ActivityTab,
    scroll: ScrollHandle,
    active_lease: Option<UiTelemetryLease>,
    _store_sub: Subscription,
}

impl ActivityView {
    /// Creates a new [`ActivityView`] starting on the `Connections` tab.
    pub fn new(
        store: Entity<UiStore>,
        commands: CommandDispatcher,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial_lease = Some(store.read(cx).acquire_telemetry(TelemetryKind::Connections));
        let store_sub = cx.subscribe(&store, |_, _, ev: &DomainChanged, cx| {
            if ev.0 == DomainKind::Activity {
                cx.notify();
            }
        });

        Self {
            store,
            commands,
            active_tab: ActivityTab::Connections,
            scroll: ScrollHandle::new(),
            active_lease: initial_lease,
            _store_sub: store_sub,
        }
    }

    fn switch_tab(&mut self, next: ActivityTab, cx: &mut Context<Self>) {
        if self.active_tab == next {
            return;
        }
        self.active_tab = next;
        // Drop previous lease before acquiring the new tab's lease.
        self.active_lease = None;
        match next {
            ActivityTab::Connections => {
                self.active_lease = Some(
                    self.store
                        .read(cx)
                        .acquire_telemetry(TelemetryKind::Connections),
                );
            }
            ActivityTab::Logs => {
                self.active_lease =
                    Some(self.store.read(cx).acquire_telemetry(TelemetryKind::Logs));
            }
            ActivityTab::Rules => {
                if self.store.read(cx).runtime().kernel_running {
                    self.commands.refresh_rules();
                }
            }
        }
        cx.notify();
    }
}

impl Render for ActivityView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let activity = self.store.read(cx).activity().clone();
        let kernel_running = self.store.read(cx).runtime().kernel_running;

        let view = cx.weak_entity();
        let switcher = ButtonGroup::connected(
            "activity-tabs",
            "Activity views",
            [
                ButtonGroupItem::new("connections", "Connections"),
                ButtonGroupItem::new("rules", "Rules"),
                ButtonGroupItem::new("logs", "Logs"),
            ],
        )
        .selection(
            ButtonGroupSelection::SingleRequired,
            [self.active_tab.id_str().into()],
        )
        .on_change(move |selected, _, cx| {
            if let Some(first) = selected.first() {
                let next = match first.as_ref() {
                    "rules" => ActivityTab::Rules,
                    "logs" => ActivityTab::Logs,
                    _ => ActivityTab::Connections,
                };
                let _ = view.update(cx, |this, cx| this.switch_tab(next, cx));
            }
        });

        let mut header = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(tokens.on_surface)
                    .child("Activity"),
            )
            .child(switcher);

        if self.active_tab == ActivityTab::Rules {
            let cmd = self.commands.clone();
            header = header.child(
                Button::new("activity-refresh-rules", "Refresh Rules")
                    .variant(ButtonVariant::Outlined)
                    .disabled(!kernel_running)
                    .on_activate(move |_, _| cmd.refresh_rules()),
            );
        }

        let body = match self.active_tab {
            ActivityTab::Connections => {
                if activity.connections.is_empty() {
                    EmptyState::new(
                        "No active connections",
                        "Live connections tracked by the running kernel appear here while this tab is open.",
                    )
                    .into_any_element()
                } else {
                    let mut card =
                        SectionCard::new()
                            .title("Active Connections")
                            .subtitle(format!(
                                "{} tracked connection(s)",
                                activity.connections.len()
                            ));
                    for conn in activity.connections.iter().take(100) {
                        let chain = conn.chains.join(" -> ");
                        let traffic = format!(
                            "↓ {} · ↑ {}",
                            format_bytes(conn.download_bytes),
                            format_bytes(conn.upload_bytes)
                        );
                        card = card.child(
                            div()
                                .p(px(10.0))
                                .rounded(px(10.0))
                                .bg(tokens.surface_container)
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_between()
                                .gap(px(12.0))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(2.0))
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(tokens.on_surface)
                                                .child(conn.host.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(tokens.on_surface_variant)
                                                .child(format!(
                                                    "{} · {} {} · {}",
                                                    conn.network,
                                                    conn.rule,
                                                    conn.rule_payload,
                                                    chain
                                                )),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(tokens.on_surface_variant)
                                        .child(traffic),
                                ),
                        );
                    }
                    card.into_any_element()
                }
            }
            ActivityTab::Rules => {
                if activity.rules.is_empty() {
                    EmptyState::new(
                        "No rules loaded",
                        "Start the kernel and refresh rules to inspect active routing rules.",
                    )
                    .into_any_element()
                } else {
                    let mut card = SectionCard::new().title("Routing Rules").subtitle(format!(
                        "{} rule(s) · {} provider(s)",
                        activity.rules.len(),
                        activity.rule_providers.len()
                    ));
                    for rule in activity.rules.iter().take(200) {
                        card = card.child(
                            div()
                                .px(px(10.0))
                                .py(px(6.0))
                                .rounded(px(8.0))
                                .bg(tokens.surface_container)
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_between()
                                .gap(px(12.0))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(tokens.on_surface)
                                        .child(format!("{}, {}", rule.kind, rule.payload)),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(tokens.primary)
                                        .child(rule.proxy.clone()),
                                ),
                        );
                    }
                    card.into_any_element()
                }
            }
            ActivityTab::Logs => {
                if activity.logs.is_empty() {
                    EmptyState::new(
                        "No kernel logs yet",
                        "Structured logs stream here on demand while the Logs tab is selected.",
                    )
                    .into_any_element()
                } else {
                    let mut card = SectionCard::new()
                        .title("Kernel Logs")
                        .subtitle(format!("{} buffered line(s)", activity.logs.len()));
                    for entry in activity.logs.iter().rev().take(200) {
                        card = card.child(
                            div()
                                .px(px(10.0))
                                .py(px(6.0))
                                .rounded(px(8.0))
                                .bg(tokens.surface_container)
                                .flex()
                                .flex_row()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(tokens.primary)
                                        .child(format!("[{:?}]", entry.level)),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .text_xs()
                                        .text_color(tokens.on_surface)
                                        .child(entry.payload.clone()),
                                ),
                        );
                    }
                    card.into_any_element()
                }
            }
        };

        div()
            .id("activity-page")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p(px(20.0))
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(header)
            .child(body)
    }
}
