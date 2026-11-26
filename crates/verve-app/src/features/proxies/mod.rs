//! Proxies feature slice: routing mode selector, proxy groups, node switching, and latency probes.

use std::time::Duration;

use gpui::{
    Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, Render,
    ScrollHandle, StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use gpui_material::{
    Button, ButtonGroup, ButtonGroupItem, ButtonGroupSelection, ButtonVariant, IconButton,
};
use gpui_material_symbols::Symbol;
use verve_core::DomainKind;
use verve_core::kernel::{ProxyGroupKind, ProxyMode};
use verve_ui_kit::{EmptyState, LatencyBadge, SectionCard, theme_tokens};

use crate::bridge::{CommandDispatcher, DomainChanged, UiStore};
use crate::shell::nav::symbol_icon;

const DEFAULT_DELAY_TEST_URL: &str = "http://www.gstatic.com/generate_204";
const DEFAULT_DELAY_TIMEOUT: Duration = Duration::from_secs(5);

/// Vertical feature view for inspecting proxy groups and switching outbound nodes.
pub struct ProxiesView {
    store: Entity<UiStore>,
    commands: CommandDispatcher,
    scroll: ScrollHandle,
    _store_sub: Subscription,
}

impl ProxiesView {
    /// Creates a new [`ProxiesView`] subscribed to `Runtime` and `Proxies` snapshot domains.
    pub fn new(
        store: Entity<UiStore>,
        commands: CommandDispatcher,
        cx: &mut Context<Self>,
    ) -> Self {
        let store_sub = cx.subscribe(&store, |_, _, ev: &DomainChanged, cx| {
            if matches!(
                ev.0,
                DomainKind::Runtime | DomainKind::Proxies | DomainKind::Settings
            ) {
                cx.notify();
            }
        });
        Self {
            store,
            commands,
            scroll: ScrollHandle::new(),
            _store_sub: store_sub,
        }
    }
}

impl Render for ProxiesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let store = self.store.read(cx);
        let runtime = store.runtime().clone();
        let proxies = store.proxies().clone();

        let selected_mode = match runtime.proxy_mode {
            ProxyMode::Rule => "rule",
            ProxyMode::Global => "global",
            ProxyMode::Direct => "direct",
        };
        let mode_commands = self.commands.clone();
        let mode_bar = ButtonGroup::connected(
            "proxies-mode-bar",
            rust_i18n::t!("common.proxy_mode_label").into_owned(),
            [
                ButtonGroupItem::new("rule", rust_i18n::t!("mode.rule").into_owned()),
                ButtonGroupItem::new("global", rust_i18n::t!("mode.global").into_owned()),
                ButtonGroupItem::new("direct", rust_i18n::t!("mode.direct").into_owned()),
            ],
        )
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

        let refresh_commands = self.commands.clone();
        let refresh_btn = Button::new(
            "proxies-refresh",
            rust_i18n::t!("common.refresh").into_owned(),
        )
        .variant(ButtonVariant::Outlined)
        .leading_icon(symbol_icon(Symbol::REFRESH))
        .on_activate(move |_, _| refresh_commands.refresh_proxies());

        let mut root = div()
            .id("proxies-page")
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
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(tokens.on_surface)
                            .child(rust_i18n::t!("proxies.title").into_owned()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(mode_bar)
                            .child(refresh_btn),
                    ),
            );

        if runtime.proxy_mode == ProxyMode::Direct {
            return root.child(EmptyState::new(
                rust_i18n::t!("proxies.direct_empty.title").into_owned(),
                rust_i18n::t!("proxies.direct_empty.desc").into_owned(),
            ));
        }

        let visible_groups: Vec<_> = proxies.groups.iter().filter(|g| !g.hidden).collect();
        if visible_groups.is_empty() {
            let desc = if runtime.kernel_running {
                rust_i18n::t!("proxies.empty.running_desc").into_owned()
            } else {
                rust_i18n::t!("proxies.empty.stopped_desc").into_owned()
            };
            return root.child(EmptyState::new(
                rust_i18n::t!("proxies.empty.title").into_owned(),
                desc,
            ));
        }

        for group in visible_groups {
            let group_name = group.name.clone();
            let active_now = group.now.clone();

            let mut grid = div().flex().flex_row().flex_wrap().gap(px(8.0));
            for (idx, member) in group.all.iter().enumerate() {
                let is_selected = active_now.as_deref() == Some(member.as_str());
                let node_meta = proxies.nodes.get(member);
                let protocol = node_meta
                    .map(|n| n.protocol.clone())
                    .unwrap_or_else(|| rust_i18n::t!("proxies.protocol_group").into_owned());
                let delay = node_meta.and_then(|n| n.latest_delay_ms);
                let udp = node_meta.is_some_and(|n| n.udp);

                let bg = if is_selected {
                    tokens.primary_container
                } else {
                    tokens.surface_container
                };
                let border = if is_selected {
                    tokens.primary
                } else {
                    tokens.outline_variant
                };
                let title_color = if is_selected {
                    tokens.on_primary_container
                } else {
                    tokens.on_surface
                };

                let select_cmd = self.commands.clone();
                let select_group = group_name.clone();
                let select_node = member.clone();

                let probe_cmd = self.commands.clone();
                let probe_node = member.clone();

                let card = div()
                    .id(("proxy-node", idx))
                    .w(px(220.0))
                    .p(px(12.0))
                    .rounded(px(12.0))
                    .bg(bg)
                    .border_1()
                    .border_color(border)
                    .cursor_pointer()
                    .on_click(move |_, _, _| {
                        select_cmd.select_proxy(select_group.clone(), select_node.clone());
                    })
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(title_color)
                                    .child(member.clone()),
                            )
                            .child(LatencyBadge::new(delay)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .child(div().text_xs().text_color(tokens.on_surface_variant).child(
                                if udp {
                                    rust_i18n::t!("proxies.protocol_udp", protocol = protocol)
                                        .into_owned()
                                } else {
                                    protocol
                                },
                            ))
                            .child(
                                IconButton::new(
                                    ("proxy-probe", idx),
                                    rust_i18n::t!("proxies.probe_tooltip", node = member)
                                        .into_owned(),
                                    symbol_icon(Symbol::SPEED),
                                )
                                .on_activate(move |_, _| {
                                    probe_cmd.test_delay(
                                        probe_node.clone(),
                                        DEFAULT_DELAY_TEST_URL,
                                        DEFAULT_DELAY_TIMEOUT,
                                    );
                                }),
                            ),
                    );

                grid = grid.child(card);
            }

            let kind_label = match &group.kind {
                ProxyGroupKind::Select => rust_i18n::t!("proxies.kind.select").into_owned(),
                ProxyGroupKind::UrlTest => rust_i18n::t!("proxies.kind.url_test").into_owned(),
                ProxyGroupKind::Fallback => rust_i18n::t!("proxies.kind.fallback").into_owned(),
                ProxyGroupKind::LoadBalance => {
                    rust_i18n::t!("proxies.kind.load_balance").into_owned()
                }
                ProxyGroupKind::Relay => rust_i18n::t!("proxies.kind.relay").into_owned(),
                ProxyGroupKind::Direct => rust_i18n::t!("proxies.kind.direct").into_owned(),
                ProxyGroupKind::Reject => rust_i18n::t!("proxies.kind.reject").into_owned(),
                ProxyGroupKind::Unknown(other) => other.clone(),
            };
            let subtitle = rust_i18n::t!(
                "proxies.group_subtitle",
                kind = kind_label,
                current = active_now.as_deref().unwrap_or("--")
            )
            .into_owned();
            root = root.child(
                SectionCard::new()
                    .title(group_name)
                    .subtitle(subtitle)
                    .child(grid),
            );
        }

        root
    }
}
