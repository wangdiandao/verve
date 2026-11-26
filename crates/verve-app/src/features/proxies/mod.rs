//! Proxies feature slice: routing mode selector, global search, collapsible proxy groups,
//! per-group sorting/locating/batch latency probing, and adaptive multi-column node grid.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use gpui::{
    AnyElement, AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, div, prelude::FluentBuilder, px,
};
use gpui_material::{
    Button, ButtonGroup, ButtonGroupItem, ButtonGroupSelection, ButtonVariant, ControlSize,
    IconButton, TextField, TextFieldEvent, TextFieldState,
};
use gpui_material_symbols::Symbol;
use verve_core::DomainKind;
use verve_core::kernel::{ProxyGroup, ProxyGroupKind, ProxyMode, ProxyNode};
use verve_ui_kit::{EmptyState, LatencyBadge, ThemeTokens, theme_tokens};

use crate::bridge::{CommandDispatcher, DomainChanged, UiStore};
use crate::shell::ShellHandle;
use crate::shell::nav::symbol_icon;

const DEFAULT_DELAY_TEST_URL: &str = "http://www.gstatic.com/generate_204";
const DEFAULT_DELAY_TIMEOUT: Duration = Duration::from_secs(5);

/// Ordering strategy applied to members within a proxy group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxySortMode {
    /// Preserve the original order reported by the proxy kernel.
    #[default]
    Default,
    /// Sort by measured latency ascending, placing untested nodes last.
    Latency,
    /// Sort alphabetically by node name (case-insensitive).
    Name,
}

impl ProxySortMode {
    /// Returns the stable string identifier for UI selection controls.
    pub const fn id_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Latency => "latency",
            Self::Name => "name",
        }
    }

    /// Parses a [`ProxySortMode`] from its stable string identifier.
    pub fn from_id_str(id: &str) -> Self {
        match id {
            "latency" => Self::Latency,
            "name" => Self::Name,
            _ => Self::Default,
        }
    }

    /// Cycles to the next sort mode (`Default` -> `Latency` -> `Name` -> `Default`).
    pub const fn next(self) -> Self {
        match self {
            Self::Default => Self::Latency,
            Self::Latency => Self::Name,
            Self::Name => Self::Default,
        }
    }
}

/// Computes the responsive card column count for the proxy node grid from the window width.
///
/// Accounts for sidebar chrome and card padding so cards always fill rows evenly in
/// 1, 2, 3, or 4 columns without awkward half-empty fixed-width wrapping.
pub fn compute_grid_columns(viewport_width_px: f32) -> usize {
    if !viewport_width_px.is_finite() || viewport_width_px < 640.0 {
        1
    } else if viewport_width_px < 980.0 {
        2
    } else if viewport_width_px < 1360.0 {
        3
    } else {
        4
    }
}

/// Returns `true` when users can manually switch the active outbound node for `kind`.
pub const fn is_group_selectable(kind: &ProxyGroupKind) -> bool {
    matches!(kind, ProxyGroupKind::Select)
}

/// Filters `members` using default case-insensitive substring/fuzzy matching against
/// node name, protocol label, and UDP capability.
///
/// Whitespace-separated query terms are combined with logical AND semantics.
pub fn filter_group_members<'a>(
    members: &'a [String],
    nodes: &BTreeMap<String, ProxyNode>,
    query: &str,
) -> Vec<&'a String> {
    let tokens: Vec<String> = query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|t| !t.is_empty())
        .collect();

    if tokens.is_empty() {
        return members.iter().collect();
    }

    members
        .iter()
        .filter(|member| {
            let name_lower = member.to_lowercase();
            let node = nodes.get(member.as_str());
            let protocol_lower = node.map(|n| n.protocol.to_lowercase()).unwrap_or_default();
            let supports_udp = node.is_some_and(|n| n.udp);

            tokens.iter().all(|tok| {
                fuzzy_subsequence_match(&name_lower, tok)
                    || (!protocol_lower.is_empty() && protocol_lower.contains(tok.as_str()))
                    || (supports_udp && tok == "udp")
            })
        })
        .collect()
}

/// Sorts filtered proxy group members according to `mode`.
///
/// - [`ProxySortMode::Default`] preserves input order.
/// - [`ProxySortMode::Latency`] orders tested nodes by ascending delay, with untested (`None`)
///   nodes placed last and ties broken deterministically by case-insensitive name.
/// - [`ProxySortMode::Name`] orders alphabetically (case-insensitive).
pub fn sort_group_members<'a>(
    mut members: Vec<&'a String>,
    nodes: &BTreeMap<String, ProxyNode>,
    mode: ProxySortMode,
) -> Vec<&'a String> {
    match mode {
        ProxySortMode::Default => members,
        ProxySortMode::Latency => {
            members.sort_by(|a, b| {
                let delay_a = nodes.get(a.as_str()).and_then(|n| n.latest_delay_ms);
                let delay_b = nodes.get(b.as_str()).and_then(|n| n.latest_delay_ms);
                delay_a
                    .is_none()
                    .cmp(&delay_b.is_none())
                    .then_with(|| {
                        delay_a
                            .unwrap_or(u16::MAX)
                            .cmp(&delay_b.unwrap_or(u16::MAX))
                    })
                    .then_with(|| a.to_lowercase().cmp(&b.to_lowercase()))
                    .then_with(|| a.cmp(b))
            });
            members
        }
        ProxySortMode::Name => {
            members.sort_by(|a, b| {
                a.to_lowercase()
                    .cmp(&b.to_lowercase())
                    .then_with(|| a.cmp(b))
            });
            members
        }
    }
}

fn fuzzy_subsequence_match(haystack_lower: &str, needle_lower: &str) -> bool {
    if haystack_lower.contains(needle_lower) {
        return true;
    }
    let mut needle_chars = needle_lower.chars();
    let Some(mut target) = needle_chars.next() else {
        return true;
    };
    for ch in haystack_lower.chars() {
        if ch == target {
            match needle_chars.next() {
                Some(next) => target = next,
                None => return true,
            }
        }
    }
    false
}

fn resolve_delay_test_url(configured: &str) -> String {
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        DEFAULT_DELAY_TEST_URL.to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn localized_group_kind(kind: &ProxyGroupKind) -> String {
    match kind {
        ProxyGroupKind::Select => rust_i18n::t!("proxies.kind.select").into_owned(),
        ProxyGroupKind::UrlTest => rust_i18n::t!("proxies.kind.url_test").into_owned(),
        ProxyGroupKind::Fallback => rust_i18n::t!("proxies.kind.fallback").into_owned(),
        ProxyGroupKind::LoadBalance => rust_i18n::t!("proxies.kind.load_balance").into_owned(),
        ProxyGroupKind::Relay => rust_i18n::t!("proxies.kind.relay").into_owned(),
        ProxyGroupKind::Direct => rust_i18n::t!("proxies.kind.direct").into_owned(),
        ProxyGroupKind::Reject => rust_i18n::t!("proxies.kind.reject").into_owned(),
        ProxyGroupKind::Unknown(other) => other.clone(),
    }
}

/// Vertical feature view for inspecting proxy groups, searching/sorting nodes,
/// switching outbound selectors, and running single/group/all latency probes.
pub struct ProxiesView {
    store: Entity<UiStore>,
    commands: CommandDispatcher,
    _shell: ShellHandle,
    scroll: ScrollHandle,
    search_state: Option<Entity<TextFieldState>>,
    _search_sub: Option<Subscription>,
    search_query: String,
    collapsed_groups: HashSet<String>,
    group_sort_modes: HashMap<String, ProxySortMode>,
    highlighted_target: Option<(String, String)>,
    _store_sub: Subscription,
}

impl ProxiesView {
    /// Creates a new [`ProxiesView`] subscribed to `Runtime`, `Proxies`, and `Settings` domains.
    pub fn new(
        store: Entity<UiStore>,
        commands: CommandDispatcher,
        shell: ShellHandle,
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
            _shell: shell,
            scroll: ScrollHandle::new(),
            search_state: None,
            _search_sub: None,
            search_query: String::new(),
            collapsed_groups: HashSet::new(),
            group_sort_modes: HashMap::new(),
            highlighted_target: None,
            _store_sub: store_sub,
        }
    }

    /// Returns `true` when the named proxy group is currently collapsed.
    pub fn is_group_collapsed(&self, group_name: &str) -> bool {
        self.collapsed_groups.contains(group_name)
    }

    /// Toggles the collapsed/expanded state of `group_name`.
    pub fn toggle_group_collapsed(
        &mut self,
        group_name: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let name = group_name.into();
        if !self.collapsed_groups.remove(&name) {
            self.collapsed_groups.insert(name);
        }
        cx.notify();
    }

    /// Returns the active [`ProxySortMode`] for `group_name`.
    pub fn group_sort_mode(&self, group_name: &str) -> ProxySortMode {
        self.group_sort_modes
            .get(group_name)
            .copied()
            .unwrap_or_default()
    }

    /// Sets the [`ProxySortMode`] for `group_name`.
    pub fn set_group_sort_mode(
        &mut self,
        group_name: impl Into<String>,
        mode: ProxySortMode,
        cx: &mut Context<Self>,
    ) {
        self.group_sort_modes.insert(group_name.into(), mode);
        cx.notify();
    }

    /// Cycles the [`ProxySortMode`] for `group_name`.
    pub fn cycle_group_sort_mode(&mut self, group_name: impl Into<String>, cx: &mut Context<Self>) {
        let name = group_name.into();
        let next = self.group_sort_mode(&name).next();
        self.group_sort_modes.insert(name, next);
        cx.notify();
    }

    /// Returns the current global search query string.
    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    /// Updates the global search query and synchronizes the retained [`TextFieldState`].
    pub fn set_search_query(
        &mut self,
        query: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let next = query.into();
        self.search_query = next.clone();
        self.highlighted_target = None;
        let state = self.ensure_search_state(window, cx);
        state.update(cx, |editor, cx| {
            editor.set_value(next, window, cx);
        });
        cx.notify();
    }

    /// Clears the global search filter and resets the search input field.
    pub fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_search_query(String::new(), window, cx);
    }

    /// Returns the `(group_name, node_name)` currently highlighted by a locate action, if any.
    pub fn highlighted_target(&self) -> Option<(&str, &str)> {
        self.highlighted_target
            .as_ref()
            .map(|(g, n)| (g.as_str(), n.as_str()))
    }

    /// Expands `group_name`, clears any search filter hiding its active node, highlights the
    /// selected node card, and emits a confirmation toast.
    pub fn locate_selected_node(
        &mut self,
        group_name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let proxies = self.store.read(cx).proxies().clone();
        let group = proxies.groups.iter().find(|g| g.name == group_name)?;
        let selected = group.now.clone()?;

        self.collapsed_groups.remove(group_name);

        if !self.search_query.trim().is_empty() {
            let single = [selected.clone()];
            let visible = filter_group_members(&single, &proxies.nodes, &self.search_query);
            if visible.is_empty() {
                self.search_query.clear();
                if let Some(state) = &self.search_state {
                    state.update(cx, |editor, cx| {
                        editor.set_value(String::new(), window, cx);
                    });
                }
            }
        }

        self.highlighted_target = Some((group_name.to_owned(), selected.clone()));
        self.commands.notify_info(
            rust_i18n::t!(
                "proxies.located_toast",
                group = group_name,
                node = selected.as_str()
            )
            .into_owned(),
        );
        cx.notify();
        Some(selected)
    }

    fn ensure_search_state(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextFieldState> {
        let desired_placeholder = rust_i18n::t!("proxies.search_placeholder").into_owned();
        if let Some(state) = &self.search_state {
            if state.read(cx).presentation().placeholder().as_ref() != desired_placeholder.as_str()
            {
                state.update(cx, |editor, cx| {
                    editor.set_placeholder(desired_placeholder, window, cx);
                });
            }
            return state.clone();
        }

        let initial_query = self.search_query.clone();
        let state = cx.new(|cx| {
            let mut editor = TextFieldState::new(window, cx)
                .placeholder(desired_placeholder)
                .clean_on_escape();
            if !initial_query.is_empty() {
                editor.set_value(initial_query, window, cx);
            }
            editor
        });
        let sub = cx.subscribe(&state, |this, state, ev: &TextFieldEvent, cx| {
            if matches!(ev, TextFieldEvent::Change) {
                let next = state.read(cx).value().to_string();
                if this.search_query != next {
                    this.search_query = next;
                    this.highlighted_target = None;
                    cx.notify();
                }
            }
        });
        self.search_state = Some(state.clone());
        self._search_sub = Some(sub);
        state
    }

    #[allow(clippy::too_many_arguments)]
    fn render_group_card(
        &self,
        group_idx: usize,
        group: &ProxyGroup,
        nodes: &BTreeMap<String, ProxyNode>,
        sorted_members: &[&String],
        cols: usize,
        search_active: bool,
        delay_url: &str,
        tokens: &ThemeTokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let group_name = group.name.clone();
        let active_now = group.now.clone();
        let is_collapsed = self.is_group_collapsed(&group_name);
        let sort_mode = self.group_sort_mode(&group_name);
        let is_selectable = is_group_selectable(&group.kind);

        let selected_delay = active_now
            .as_deref()
            .and_then(|name| nodes.get(name))
            .and_then(|node| node.latest_delay_ms);

        let kind_label = localized_group_kind(&group.kind);
        let count_label = if search_active {
            rust_i18n::t!(
                "proxies.group_count_filtered",
                visible = sorted_members.len(),
                total = group.all.len()
            )
            .into_owned()
        } else {
            group.all.len().to_string()
        };

        let chevron_symbol = if is_collapsed {
            Symbol::CHEVRON_RIGHT
        } else {
            Symbol::EXPAND_MORE
        };

        let toggle_view = cx.weak_entity();
        let toggle_group = group_name.clone();
        let header_leading = div()
            .id(SharedString::from(format!(
                "proxy-group-toggle-{group_idx}"
            )))
            .flex_1()
            .min_w(px(200.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .py(px(2.0))
            .rounded(px(8.0))
            .cursor_pointer()
            .on_click(move |_, _, cx| {
                let _ = toggle_view.update(cx, |this, cx| {
                    this.toggle_group_collapsed(toggle_group.clone(), cx);
                });
            })
            .child(
                div()
                    .flex_none()
                    .text_color(tokens.on_surface_variant)
                    .child(symbol_icon(chevron_symbol).size(px(20.0))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(tokens.on_surface)
                                    .child(group_name.clone()),
                            )
                            .child(
                                div()
                                    .px(px(8.0))
                                    .py(px(2.0))
                                    .rounded_full()
                                    .bg(tokens.secondary_container)
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(tokens.on_secondary_container)
                                    .child(kind_label),
                            )
                            .child(
                                div()
                                    .px(px(8.0))
                                    .py(px(2.0))
                                    .rounded_full()
                                    .bg(tokens.surface_container_high)
                                    .text_xs()
                                    .text_color(tokens.on_surface_variant)
                                    .child(count_label),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .min_w_0()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(tokens.on_surface_variant)
                                    .child(
                                        rust_i18n::t!(
                                            "proxies.group_current",
                                            current = active_now.as_deref().unwrap_or("--")
                                        )
                                        .into_owned(),
                                    ),
                            )
                            .when(active_now.is_some(), |row| {
                                row.child(LatencyBadge::new(selected_delay))
                            }),
                    ),
            );

        let sort_view = cx.weak_entity();
        let sort_group = group_name.clone();
        let sort_control = ButtonGroup::connected(
            SharedString::from(format!("proxy-group-sort-{group_idx}")),
            rust_i18n::t!("proxies.sort_label").into_owned(),
            [
                ButtonGroupItem::new(
                    "default",
                    rust_i18n::t!("proxies.sort.default").into_owned(),
                ),
                ButtonGroupItem::new(
                    "latency",
                    rust_i18n::t!("proxies.sort.latency").into_owned(),
                ),
                ButtonGroupItem::new("name", rust_i18n::t!("proxies.sort.name").into_owned()),
            ],
        )
        .size(ControlSize::ExtraSmall)
        .selection(
            ButtonGroupSelection::SingleRequired,
            [sort_mode.id_str().into()],
        )
        .on_change(move |selected, _, cx| {
            if let Some(first) = selected.first() {
                let next_mode = ProxySortMode::from_id_str(first.as_ref());
                let _ = sort_view.update(cx, |this, cx| {
                    this.set_group_sort_mode(sort_group.clone(), next_mode, cx);
                });
            }
        });

        let locate_view = cx.weak_entity();
        let locate_group = group_name.clone();
        let locate_btn = IconButton::new(
            SharedString::from(format!("proxy-group-locate-{group_idx}")),
            rust_i18n::t!("proxies.locate_tooltip").into_owned(),
            symbol_icon(Symbol::MY_LOCATION),
        )
        .size(ControlSize::ExtraSmall)
        .disabled(active_now.is_none())
        .on_activate(move |window, cx| {
            let _ = locate_view.update(cx, |this, cx| {
                this.locate_selected_node(&locate_group, window, cx);
            });
        });

        let group_test_cmd = self.commands.clone();
        let group_test_name = group_name.clone();
        let group_test_url = delay_url.to_owned();
        let group_test_btn = IconButton::new(
            SharedString::from(format!("proxy-group-test-{group_idx}")),
            rust_i18n::t!("proxies.test_group_tooltip", group = group_name.as_str()).into_owned(),
            symbol_icon(Symbol::SPEED),
        )
        .size(ControlSize::ExtraSmall)
        .on_activate(move |_, _| {
            group_test_cmd.test_group_delay(
                group_test_name.clone(),
                group_test_url.clone(),
                DEFAULT_DELAY_TIMEOUT,
            );
            group_test_cmd.notify_info(
                rust_i18n::t!(
                    "proxies.testing_group_toast",
                    group = group_test_name.as_str()
                )
                .into_owned(),
            );
        });

        let header = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .child(header_leading)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_none()
                    .items_center()
                    .gap(px(4.0))
                    .child(sort_control)
                    .child(locate_btn)
                    .child(group_test_btn),
            );

        let mut card = div()
            .id(SharedString::from(format!("proxy-group-{group_idx}")))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .p(px(16.0))
            .rounded(px(16.0))
            .bg(tokens.surface_container_low)
            .border_1()
            .border_color(tokens.outline_variant)
            .child(header);

        if !is_collapsed {
            let mut grid = div().flex().flex_col().gap(px(8.0)).w_full();
            for (row_idx, row_members) in sorted_members.chunks(cols).enumerate() {
                let mut row = div().w_full().flex().flex_row().gap(px(8.0));
                for (col_idx, member) in row_members.iter().enumerate() {
                    let member_idx = row_idx * cols + col_idx;
                    let is_selected = active_now.as_deref() == Some(member.as_str());
                    let is_highlighted =
                        self.highlighted_target() == Some((group_name.as_str(), member.as_str()));

                    let node_meta = nodes.get(member.as_str());
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
                    let title_color = if is_selected {
                        tokens.on_primary_container
                    } else {
                        tokens.on_surface
                    };
                    let subtitle_color = if is_selected {
                        tokens.on_primary_container
                    } else {
                        tokens.on_surface_variant
                    };

                    let select_cmd = self.commands.clone();
                    let select_view = cx.weak_entity();
                    let select_group = group_name.clone();
                    let select_node = (*member).clone();

                    let probe_cmd = self.commands.clone();
                    let probe_node = (*member).clone();
                    let probe_url = delay_url.to_owned();

                    let protocol_text = if udp {
                        rust_i18n::t!("proxies.protocol_udp", protocol = protocol).into_owned()
                    } else {
                        protocol
                    };

                    let node_card = div()
                        .id(SharedString::from(format!(
                            "proxy-node-{group_idx}-{member_idx}"
                        )))
                        .flex_1()
                        .min_w_0()
                        .p(px(12.0))
                        .rounded(px(12.0))
                        .bg(bg)
                        .when_else(
                            is_highlighted,
                            |c| c.border_2().border_color(tokens.tertiary),
                            |c| {
                                c.border_1().border_color(if is_selected {
                                    tokens.primary
                                } else {
                                    tokens.outline_variant
                                })
                            },
                        )
                        .when(is_selectable, |c| {
                            c.cursor_pointer().on_click(move |_, _, cx| {
                                let _ = select_view.update(cx, |this, cx| {
                                    this.highlighted_target = None;
                                    cx.notify();
                                });
                                select_cmd.select_proxy(select_group.clone(), select_node.clone());
                            })
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
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(title_color)
                                        .child((*member).clone()),
                                )
                                .child(LatencyBadge::new(delay)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_between()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .text_color(subtitle_color)
                                        .child(protocol_text),
                                )
                                .child(
                                    IconButton::new(
                                        SharedString::from(format!(
                                            "proxy-probe-{group_idx}-{member_idx}"
                                        )),
                                        rust_i18n::t!("proxies.probe_tooltip", node = *member)
                                            .into_owned(),
                                        symbol_icon(Symbol::SPEED),
                                    )
                                    .size(ControlSize::ExtraSmall)
                                    .on_activate(
                                        move |_, _| {
                                            probe_cmd.test_delay(
                                                probe_node.clone(),
                                                probe_url.clone(),
                                                DEFAULT_DELAY_TIMEOUT,
                                            );
                                        },
                                    ),
                                ),
                        );

                    row = row.child(node_card);
                }

                for _ in row_members.len()..cols {
                    row = row.child(div().flex_1().min_w_0());
                }

                grid = grid.child(row);
            }
            card = card.child(grid);
        }

        card.into_any_element()
    }
}

impl Render for ProxiesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let search_state = self.ensure_search_state(window, cx);
        let tokens = theme_tokens(cx);
        let store = self.store.read(cx);
        let runtime = store.runtime().clone();
        let proxies = store.proxies().clone();
        let settings = store.settings().clone();
        let delay_url = resolve_delay_test_url(&settings.syncable.default_delay_test_url);

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

        let visible_groups: Vec<&ProxyGroup> =
            proxies.groups.iter().filter(|g| !g.hidden).collect();

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
                    .child(mode_bar),
            );

        if runtime.proxy_mode == ProxyMode::Direct {
            let switch_cmd = self.commands.clone();
            return root.child(
                EmptyState::new(
                    rust_i18n::t!("proxies.direct_empty.title").into_owned(),
                    rust_i18n::t!("proxies.direct_empty.desc").into_owned(),
                )
                .action(
                    Button::new(
                        "proxies-switch-rule",
                        rust_i18n::t!("proxies.direct_empty.switch_rule").into_owned(),
                    )
                    .variant(ButtonVariant::FilledTonal)
                    .on_activate(move |_, _| {
                        switch_cmd.set_proxy_mode(ProxyMode::Rule);
                    }),
                ),
            );
        }

        let search_field = div().flex_1().min_w(px(200.0)).max_w(px(420.0)).child(
            TextField::outlined(
                "proxies-search",
                rust_i18n::t!("proxies.search_label").into_owned(),
                &search_state,
            )
            .show_label(false)
            .leading(symbol_icon(Symbol::SEARCH))
            .clearable(true)
            .clear_label(rust_i18n::t!("proxies.clear_search").into_owned())
            .min_height(px(40.0)),
        );

        let test_all_commands = self.commands.clone();
        let test_all_url = delay_url.clone();
        let test_all_btn = Button::new(
            "proxies-test-all",
            rust_i18n::t!("proxies.test_all").into_owned(),
        )
        .variant(ButtonVariant::FilledTonal)
        .leading_icon(symbol_icon(Symbol::SPEED))
        .disabled(visible_groups.is_empty())
        .on_activate(move |_, _| {
            test_all_commands.test_all_groups_delay(test_all_url.clone(), DEFAULT_DELAY_TIMEOUT);
            test_all_commands.notify_info(rust_i18n::t!("proxies.testing_all_toast").into_owned());
        });

        let refresh_commands = self.commands.clone();
        let refresh_btn = Button::new(
            "proxies-refresh",
            rust_i18n::t!("common.refresh").into_owned(),
        )
        .variant(ButtonVariant::Outlined)
        .leading_icon(symbol_icon(Symbol::REFRESH))
        .on_activate(move |_, _| refresh_commands.refresh_proxies());

        root = root.child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(px(12.0))
                .child(search_field)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .child(test_all_btn)
                        .child(refresh_btn),
                ),
        );

        if visible_groups.is_empty() {
            let (desc, action) = if runtime.kernel_running {
                let cmd = self.commands.clone();
                (
                    rust_i18n::t!("proxies.empty.running_desc").into_owned(),
                    Button::new(
                        "proxies-empty-refresh",
                        rust_i18n::t!("common.refresh").into_owned(),
                    )
                    .variant(ButtonVariant::FilledTonal)
                    .leading_icon(symbol_icon(Symbol::REFRESH))
                    .on_activate(move |_, _| cmd.refresh_proxies()),
                )
            } else {
                let cmd = self.commands.clone();
                (
                    rust_i18n::t!("proxies.empty.stopped_desc").into_owned(),
                    Button::new(
                        "proxies-start-kernel",
                        rust_i18n::t!("proxies.empty.start_kernel").into_owned(),
                    )
                    .variant(ButtonVariant::Filled)
                    .leading_icon(symbol_icon(Symbol::PLAY_ARROW))
                    .on_activate(move |_, _| cmd.start_kernel()),
                )
            };
            return root.child(
                EmptyState::new(rust_i18n::t!("proxies.empty.title").into_owned(), desc)
                    .action(action),
            );
        }

        let trimmed_query = self.search_query.trim();
        let search_active = !trimmed_query.is_empty();
        let cols = compute_grid_columns(f32::from(window.viewport_size().width));
        let mut rendered_any_group = false;

        for (group_idx, group) in visible_groups.iter().enumerate() {
            let filtered = filter_group_members(&group.all, &proxies.nodes, &self.search_query);
            if search_active && filtered.is_empty() {
                continue;
            }
            rendered_any_group = true;
            let sort_mode = self.group_sort_mode(&group.name);
            let sorted = sort_group_members(filtered, &proxies.nodes, sort_mode);

            root = root.child(self.render_group_card(
                group_idx,
                group,
                &proxies.nodes,
                &sorted,
                cols,
                search_active,
                &delay_url,
                &tokens,
                cx,
            ));
        }

        if search_active && !rendered_any_group {
            let clear_view = cx.weak_entity();
            return root.child(
                EmptyState::new(
                    rust_i18n::t!("proxies.search_empty.title").into_owned(),
                    rust_i18n::t!("proxies.search_empty.desc", query = trimmed_query).into_owned(),
                )
                .action(
                    Button::new(
                        "proxies-clear-search",
                        rust_i18n::t!("proxies.clear_search").into_owned(),
                    )
                    .variant(ButtonVariant::FilledTonal)
                    .leading_icon(symbol_icon(Symbol::CLOSE))
                    .on_activate(move |window, cx| {
                        let _ = clear_view.update(cx, |this, cx| {
                            this.clear_search(window, cx);
                        });
                    }),
                ),
            );
        }

        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_nodes() -> BTreeMap<String, ProxyNode> {
        let mut map = BTreeMap::new();
        map.insert(
            "HK-01".into(),
            ProxyNode {
                name: "HK-01".into(),
                protocol: "vless".into(),
                udp: true,
                latest_delay_ms: Some(45),
                alive: true,
            },
        );
        map.insert(
            "us-west-02".into(),
            ProxyNode {
                name: "us-west-02".into(),
                protocol: "trojan".into(),
                udp: false,
                latest_delay_ms: Some(180),
                alive: true,
            },
        );
        map.insert(
            "JP-Tokyo-01".into(),
            ProxyNode {
                name: "JP-Tokyo-01".into(),
                protocol: "Shadowsocks".into(),
                udp: true,
                latest_delay_ms: Some(28),
                alive: true,
            },
        );
        map.insert(
            "SG-Untested".into(),
            ProxyNode {
                name: "SG-Untested".into(),
                protocol: "vmess".into(),
                udp: false,
                latest_delay_ms: None,
                alive: true,
            },
        );
        map
    }

    #[test]
    fn filter_matches_case_insensitively_by_name_fuzzy_protocol_and_udp() {
        let nodes = sample_nodes();
        let members: Vec<String> = vec![
            "HK-01".into(),
            "us-west-02".into(),
            "JP-Tokyo-01".into(),
            "SG-Untested".into(),
        ];

        assert_eq!(filter_group_members(&members, &nodes, "").len(), 4);
        assert_eq!(filter_group_members(&members, &nodes, "   ").len(), 4);

        // Case-insensitive substring on name.
        let hk = filter_group_members(&members, &nodes, "hk");
        assert_eq!(hk, vec![&members[0]]);

        // Fuzzy subsequence match on name (`jp1` -> `JP-Tokyo-01`).
        let jp = filter_group_members(&members, &nodes, "jp1");
        assert_eq!(jp, vec![&members[2]]);

        // Case-insensitive substring on protocol.
        let ss = filter_group_members(&members, &nodes, "SHADOWSOCKS");
        assert_eq!(ss, vec![&members[2]]);

        // Multi-token AND match across name + protocol.
        let multi = filter_group_members(&members, &nodes, "us troj");
        assert_eq!(multi, vec![&members[1]]);

        // UDP capability token.
        let udp_only = filter_group_members(&members, &nodes, "udp");
        assert_eq!(udp_only, vec![&members[0], &members[2]]);

        // Non-matching query yields empty result.
        assert!(filter_group_members(&members, &nodes, "nonexistent-zz").is_empty());
    }

    #[test]
    fn sort_orders_default_latency_with_untested_last_and_name() {
        let nodes = sample_nodes();
        let members: Vec<String> = vec![
            "SG-Untested".into(),
            "us-west-02".into(),
            "HK-01".into(),
            "JP-Tokyo-01".into(),
            "DIRECT".into(),
        ];
        let refs: Vec<&String> = members.iter().collect();

        // Default preserves exact input order.
        let default_sorted = sort_group_members(refs.clone(), &nodes, ProxySortMode::Default);
        let default_names: Vec<&str> = default_sorted.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            default_names,
            vec![
                "SG-Untested",
                "us-west-02",
                "HK-01",
                "JP-Tokyo-01",
                "DIRECT"
            ]
        );

        // Latency puts tested nodes ascending first, then untested (`None`) sorted alphabetically.
        let latency_sorted = sort_group_members(refs.clone(), &nodes, ProxySortMode::Latency);
        let latency_names: Vec<&str> = latency_sorted.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            latency_names,
            vec![
                "JP-Tokyo-01",
                "HK-01",
                "us-west-02",
                "DIRECT",
                "SG-Untested"
            ]
        );

        // Name sorts case-insensitively A-Z.
        let name_sorted = sort_group_members(refs, &nodes, ProxySortMode::Name);
        let alpha_names: Vec<&str> = name_sorted.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            alpha_names,
            vec![
                "DIRECT",
                "HK-01",
                "JP-Tokyo-01",
                "SG-Untested",
                "us-west-02"
            ]
        );
    }

    #[test]
    fn responsive_grid_columns_scales_smoothly_from_one_to_four() {
        assert_eq!(compute_grid_columns(0.0), 1);
        assert_eq!(compute_grid_columns(f32::NAN), 1);
        assert_eq!(compute_grid_columns(500.0), 1);
        assert_eq!(compute_grid_columns(640.0), 2);
        assert_eq!(compute_grid_columns(760.0), 2);
        assert_eq!(compute_grid_columns(979.0), 2);
        assert_eq!(compute_grid_columns(980.0), 3);
        assert_eq!(compute_grid_columns(1080.0), 3);
        assert_eq!(compute_grid_columns(1359.0), 3);
        assert_eq!(compute_grid_columns(1360.0), 4);
        assert_eq!(compute_grid_columns(1920.0), 4);
    }

    #[test]
    fn sort_mode_round_trips_and_cycles() {
        assert_eq!(ProxySortMode::Default.next(), ProxySortMode::Latency);
        assert_eq!(ProxySortMode::Latency.next(), ProxySortMode::Name);
        assert_eq!(ProxySortMode::Name.next(), ProxySortMode::Default);

        for mode in [
            ProxySortMode::Default,
            ProxySortMode::Latency,
            ProxySortMode::Name,
        ] {
            assert_eq!(ProxySortMode::from_id_str(mode.id_str()), mode);
        }
        assert_eq!(
            ProxySortMode::from_id_str("unknown"),
            ProxySortMode::Default
        );
    }
}
