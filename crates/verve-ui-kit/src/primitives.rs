//! Stateless Material 3 Expressive primitive components for Verve.

use gpui::{
    AnyElement, App, FontWeight, IntoElement, ParentElement, RenderOnce, SharedString, Styled,
    Window, div, px,
};
use gpui_material::theme::ColorRole;

use crate::theme::{KernelHealthState, LatencyBand, role_color, theme_tokens};

/// Rounded Material 3 section container (`surface_container_low`, `16px` radius, `1px` outline).
#[derive(IntoElement)]
pub struct SectionCard {
    title: Option<SharedString>,
    subtitle: Option<SharedString>,
    header_trailing: Option<AnyElement>,
    children: Vec<AnyElement>,
}

impl Default for SectionCard {
    fn default() -> Self {
        Self::new()
    }
}

impl SectionCard {
    /// Creates an empty Material 3 section card.
    pub fn new() -> Self {
        Self {
            title: None,
            subtitle: None,
            header_trailing: None,
            children: Vec::new(),
        }
    }

    /// Sets an optional card title rendered in the top header row.
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Sets an optional muted subtitle rendered below [`Self::title`].
    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// Sets an optional trailing action or badge element aligned to the header's right edge.
    pub fn header_trailing(mut self, trailing: impl IntoElement) -> Self {
        self.header_trailing = Some(trailing.into_any_element());
        self
    }
}

impl ParentElement for SectionCard {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for SectionCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = theme_tokens(cx);

        let mut card = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .p(px(16.0))
            .rounded(px(16.0))
            .bg(tokens.surface_container_low)
            .border_1()
            .border_color(tokens.outline_variant);

        let has_header =
            self.title.is_some() || self.subtitle.is_some() || self.header_trailing.is_some();

        if has_header {
            let mut title_col = div().flex().flex_col().gap(px(2.0));
            if let Some(title) = self.title {
                title_col = title_col.child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(tokens.on_surface)
                        .child(title),
                );
            }
            if let Some(subtitle) = self.subtitle {
                title_col = title_col.child(
                    div()
                        .text_xs()
                        .text_color(tokens.on_surface_variant)
                        .child(subtitle),
                );
            }

            let mut header = div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(12.0))
                .child(title_col);

            if let Some(trailing) = self.header_trailing {
                header = header.child(trailing);
            }

            card = card.child(header);
        }

        if !self.children.is_empty() {
            card = card.children(self.children);
        }

        card
    }
}

/// Compact pill displaying a colored status dot and kernel health label.
#[derive(IntoElement)]
pub struct HealthIndicator {
    state: KernelHealthState,
    label: Option<SharedString>,
}

impl HealthIndicator {
    /// Creates a status indicator for `state` using its default label.
    pub fn new(state: KernelHealthState) -> Self {
        Self { state, label: None }
    }

    /// Overrides the displayed status text (e.g. for localized labels or kernel version suffixes).
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl RenderOnce for HealthIndicator {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let (dot_color, bg_color, text_color) = self.state.pill_colors(&tokens);
        let text = self
            .label
            .unwrap_or_else(|| SharedString::from(self.state.default_label()));

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .px(px(10.0))
            .py(px(4.0))
            .rounded_full()
            .bg(bg_color)
            .child(div().size(px(8.0)).rounded_full().bg(dot_color))
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(text_color)
                    .child(text),
            )
    }
}

/// Compact latency pill displaying `"42 ms"` or `"--"` tinted by [`LatencyBand`].
#[derive(IntoElement)]
pub struct LatencyBadge {
    delay_ms: Option<u16>,
}

impl LatencyBadge {
    /// Creates a latency badge for an optional delay measurement in milliseconds.
    pub fn new(delay_ms: Option<u16>) -> Self {
        Self { delay_ms }
    }

    /// Returns the measured delay in milliseconds, if available.
    pub const fn delay_ms(&self) -> Option<u16> {
        self.delay_ms
    }

    /// Returns the resolved [`LatencyBand`] for this badge.
    pub const fn band(&self) -> LatencyBand {
        LatencyBand::from_delay_ms(self.delay_ms)
    }

    /// Formats the badge display string (`"<ms> ms"` or `"--"`).
    pub fn formatted_text(&self) -> String {
        match self.delay_ms {
            Some(ms) => format!("{ms} ms"),
            None => "--".to_owned(),
        }
    }
}

impl RenderOnce for LatencyBadge {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let (bg_color, text_color) = self.band().badge_colors(&tokens);

        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .px(px(8.0))
            .py(px(2.0))
            .rounded_full()
            .bg(bg_color)
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(text_color)
            .child(self.formatted_text())
    }
}

/// Compact key-value telemetry tile for rates, totals, memory usage, and ports.
#[derive(IntoElement)]
pub struct MetricTile {
    label: SharedString,
    value: SharedString,
    sublabel: Option<SharedString>,
    value_role: Option<ColorRole>,
}

impl MetricTile {
    /// Creates a metric tile with a muted label and prominent value.
    pub fn new(label: impl Into<SharedString>, value: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            sublabel: None,
            value_role: None,
        }
    }

    /// Sets an optional secondary line beneath the metric value.
    pub fn sublabel(mut self, sublabel: impl Into<SharedString>) -> Self {
        self.sublabel = Some(sublabel.into());
        self
    }

    /// Overrides the metric value text color using a Material [`ColorRole`].
    pub fn value_role(mut self, role: ColorRole) -> Self {
        self.value_role = Some(role);
        self
    }
}

impl RenderOnce for MetricTile {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = gpui_material::material_theme(cx);
        let tokens = theme_tokens(cx);
        let value_color = self
            .value_role
            .map_or(tokens.on_surface, |role| role_color(theme, role));

        let mut tile = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .px(px(12.0))
            .py(px(10.0))
            .rounded(px(12.0))
            .bg(tokens.surface_container)
            .border_1()
            .border_color(tokens.outline_variant)
            .child(
                div()
                    .text_xs()
                    .text_color(tokens.on_surface_variant)
                    .child(self.label),
            )
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(value_color)
                    .child(self.value),
            );

        if let Some(sublabel) = self.sublabel {
            tile = tile.child(
                div()
                    .text_xs()
                    .text_color(tokens.on_surface_variant)
                    .child(sublabel),
            );
        }

        tile
    }
}

/// Centered Material 3 empty-state placeholder with heading, description, and optional action.
#[derive(IntoElement)]
pub struct EmptyState {
    heading: SharedString,
    description: SharedString,
    action: Option<AnyElement>,
}

impl EmptyState {
    /// Creates an empty-state placeholder with a heading and explanatory description.
    pub fn new(heading: impl Into<SharedString>, description: impl Into<SharedString>) -> Self {
        Self {
            heading: heading.into(),
            description: description.into(),
            action: None,
        }
    }

    /// Attaches an optional action element (e.g. a button or link) beneath the description.
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }
}

impl RenderOnce for EmptyState {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = theme_tokens(cx);

        let mut container = div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .p(px(24.0))
            .rounded(px(16.0))
            .bg(tokens.surface_container_lowest)
            .border_1()
            .border_color(tokens.outline_variant)
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(tokens.on_surface)
                    .child(self.heading),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(tokens.on_surface_variant)
                    .child(self.description),
            );

        if let Some(action) = self.action {
            container = container.child(div().pt(px(4.0)).child(action));
        }

        container
    }
}
