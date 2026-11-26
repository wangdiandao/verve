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
            if matches!(ev.0, DomainKind::Activity | DomainKind::Settings) {
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
            rust_i18n::t!("activity.tabs_label").into_owned(),
            [
                ButtonGroupItem::new(
                    "connections",
                    rust_i18n::t!("activity.tab.connections").into_owned(),
                ),
                ButtonGroupItem::new("rules", rust_i18n::t!("activity.tab.rules").into_owned()),
                ButtonGroupItem::new("logs", rust_i18n::t!("activity.tab.logs").into_owned()),
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
                    .child(rust_i18n::t!("activity.title").into_owned()),
            )
            .child(switcher);

        if self.active_tab == ActivityTab::Rules {
            let cmd = self.commands.clone();
            header = header.child(
                Button::new(
                    "activity-refresh-rules",
                    rust_i18n::t!("activity.refresh_rules").into_owned(),
                )
                .variant(ButtonVariant::Outlined)
                .disabled(!kernel_running)
                .on_activate(move |_, _| cmd.refresh_rules()),
            );
        }

        let body = match self.active_tab {
            ActivityTab::Connections => {
                if activity.connections.is_empty() {
                    EmptyState::new(
                        rust_i18n::t!("activity.connections.empty_title").into_owned(),
                        "",
                    )
                    .into_any_element()
                } else {
                    let mut card = SectionCard::new()
                        .title(rust_i18n::t!("activity.connections.card_title").into_owned())
                        .subtitle(
                            rust_i18n::t!(
                                "activity.connections.card_subtitle",
                                count = activity.connections.len()
                            )
                            .into_owned(),
                        );
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
                    EmptyState::new(rust_i18n::t!("activity.rules.empty_title").into_owned(), "")
                        .into_any_element()
                } else {
                    let mut card = SectionCard::new()
                        .title(rust_i18n::t!("activity.rules.card_title").into_owned())
                        .subtitle(
                            rust_i18n::t!(
                                "activity.rules.card_subtitle",
                                rules = activity.rules.len(),
                                providers = activity.rule_providers.len()
                            )
                            .into_owned(),
                        );
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
                    EmptyState::new(rust_i18n::t!("activity.logs.empty_title").into_owned(), "")
                        .into_any_element()
                } else {
                    let mut card = SectionCard::new()
                        .title(rust_i18n::t!("activity.logs.card_title").into_owned())
                        .subtitle(
                            rust_i18n::t!(
                                "activity.logs.card_subtitle",
                                count = activity.logs.len()
                            )
                            .into_owned(),
                        );
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
