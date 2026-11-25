//! Settings feature slice: network/service parameters and appearance mode selector.

use gpui::{
    Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, Render,
    ScrollHandle, StatefulInteractiveElement, Styled, Subscription, Window, WindowAppearance, div,
    px,
};
use gpui_material::{ButtonGroup, ButtonGroupItem, ButtonGroupSelection};
use verve_core::DomainKind;
use verve_ui_kit::{
    AppearanceMode, DEFAULT_SEED_ARGB, MetricTile, SectionCard, apply_theme, build_material_theme,
    theme_tokens,
};

use crate::bridge::{CommandDispatcher, DomainChanged, UiStore};

/// Vertical feature view projecting [`verve_core::SettingsSlice`] (`SyncableConfig` + `LocalMachineConfig`).
pub struct SettingsView {
    store: Entity<UiStore>,
    commands: CommandDispatcher,
    scroll: ScrollHandle,
    _store_sub: Subscription,
}

impl SettingsView {
    /// Creates a new [`SettingsView`] subscribed to `Settings` slice updates.
    pub fn new(
        store: Entity<UiStore>,
        commands: CommandDispatcher,
        cx: &mut Context<Self>,
    ) -> Self {
        let store_sub = cx.subscribe(&store, |_, _, ev: &DomainChanged, cx| {
            if ev.0 == DomainKind::Settings {
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

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let settings = self.store.read(cx).settings().clone();
        let syncable = &settings.syncable;
        let local = &settings.local;

        let current_mode =
            AppearanceMode::from_config_str(&syncable.theme_mode).unwrap_or(AppearanceMode::System);
        let theme_commands = self.commands.clone();
        let appearance_selector = ButtonGroup::connected(
            "settings-theme-mode",
            "Theme mode",
            [
                ButtonGroupItem::new("light", "Light"),
                ButtonGroupItem::new("dark", "Dark"),
                ButtonGroupItem::new("system", "System"),
            ],
        )
        .selection(
            ButtonGroupSelection::SingleRequired,
            [current_mode.as_config_str().into()],
        )
        .on_change(move |selected, _, cx| {
            if let Some(first) = selected.first()
                && let Some(mode) = AppearanceMode::from_config_str(first.as_ref())
            {
                let system_dark = matches!(
                    cx.window_appearance(),
                    WindowAppearance::Dark | WindowAppearance::VibrantDark
                );
                apply_theme(
                    build_material_theme(mode, Some(DEFAULT_SEED_ARGB), system_dark),
                    cx,
                );
                theme_commands.set_theme_mode(mode.as_config_str());
            }
        });

        let bypass_summary = if syncable.system_proxy_bypass.is_empty() {
            "Default bypass rules".to_owned()
        } else {
            syncable.system_proxy_bypass.join(", ")
        };

        div()
            .id("settings-page")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p(px(20.0))
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(tokens.on_surface)
                    .child("Settings"),
            )
            .child(
                SectionCard::new()
                    .title("Appearance")
                    .subtitle("Material 3 dynamic color palette and theme mode")
                    .child(appearance_selector),
            )
            .child(
                SectionCard::new()
                    .title("Network & Inbound")
                    .subtitle("Persisted syncable network preferences (settings.toml)")
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(px(12.0))
                            .child(MetricTile::new(
                                "Mixed Port",
                                syncable.mixed_port.to_string(),
                            ))
                            .child(MetricTile::new(
                                "Allow LAN",
                                if syncable.allow_lan {
                                    "Enabled"
                                } else {
                                    "Disabled"
                                },
                            ))
                            .child(MetricTile::new(
                                "IPv6",
                                if syncable.ipv6 { "Enabled" } else { "Disabled" },
                            ))
                            .child(MetricTile::new("Log Level", syncable.log_level.clone())),
                    )
                    .child(MetricTile::new("System Proxy Bypass", bypass_summary)),
            )
            .child(
                SectionCard::new()
                    .title("Machine & Service")
                    .subtitle("Local machine settings (local.toml)")
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(px(12.0))
                            .child(MetricTile::new(
                                "Service Mode",
                                if local.service_mode_enabled {
                                    "Enabled"
                                } else {
                                    "Disabled (Direct)"
                                },
                            ))
                            .child(MetricTile::new(
                                "TUN Interface",
                                local
                                    .tun_interface_name
                                    .clone()
                                    .unwrap_or_else(|| "Auto".into()),
                            ))
                            .child(MetricTile::new(
                                "Auto Launch",
                                if local.auto_launch {
                                    "Enabled"
                                } else {
                                    "Disabled"
                                },
                            )),
                    ),
            )
    }
}
