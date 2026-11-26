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
use crate::shell::ShellHandle;

/// Vertical feature view projecting [`verve_core::SettingsSlice`] (`SyncableConfig` + `LocalMachineConfig`).
pub struct SettingsView {
    store: Entity<UiStore>,
    commands: CommandDispatcher,
    _shell: ShellHandle,
    scroll: ScrollHandle,
    _store_sub: Subscription,
}

impl SettingsView {
    /// Creates a new [`SettingsView`] subscribed to `Settings` slice updates.
    pub fn new(
        store: Entity<UiStore>,
        commands: CommandDispatcher,
        shell: ShellHandle,
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
            _shell: shell,
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
        let current_seed = syncable.theme_seed_color.or(Some(DEFAULT_SEED_ARGB));
        let theme_commands = self.commands.clone();
        let appearance_selector = ButtonGroup::connected(
            "settings-theme-mode",
            rust_i18n::t!("settings.theme_mode_label").into_owned(),
            [
                ButtonGroupItem::new("light", rust_i18n::t!("settings.theme.light").into_owned()),
                ButtonGroupItem::new("dark", rust_i18n::t!("settings.theme.dark").into_owned()),
                ButtonGroupItem::new(
                    "system",
                    rust_i18n::t!("settings.theme.system").into_owned(),
                ),
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
                apply_theme(build_material_theme(mode, current_seed, system_dark), cx);
                theme_commands.set_theme_mode(mode.as_config_str());
            }
        });

        let current_locale =
            crate::i18n::AppLocale::from_config_str(&syncable.locale).unwrap_or_default();
        let locale_commands = self.commands.clone();
        let locale_selector = ButtonGroup::connected(
            "settings-locale",
            rust_i18n::t!("settings.language_label").into_owned(),
            [
                ButtonGroupItem::new("zh-CN", rust_i18n::t!("settings.locale.zh_cn").into_owned()),
                ButtonGroupItem::new("en-US", rust_i18n::t!("settings.locale.en_us").into_owned()),
            ],
        )
        .selection(
            ButtonGroupSelection::SingleRequired,
            [current_locale.as_config_str().into()],
        )
        .on_change(move |selected, _, cx| {
            if let Some(first) = selected.first()
                && let Some(locale) = crate::i18n::AppLocale::from_config_str(first.as_ref())
            {
                let tag = locale.as_config_str();
                crate::i18n::apply_locale(tag);
                locale_commands.set_locale(tag);
                cx.refresh_windows();
            }
        });

        let enabled_label = rust_i18n::t!("common.enabled").into_owned();
        let disabled_label = rust_i18n::t!("common.disabled").into_owned();
        let bypass_summary = if syncable.system_proxy_bypass.is_empty() {
            rust_i18n::t!("settings.network.default_bypass").into_owned()
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
                    .child(rust_i18n::t!("settings.title").into_owned()),
            )
            .child(
                SectionCard::new()
                    .title(rust_i18n::t!("settings.appearance.title").into_owned())
                    .subtitle(rust_i18n::t!("settings.appearance.subtitle").into_owned())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(tokens.on_surface_variant)
                                    .child(rust_i18n::t!("settings.theme_mode_label").into_owned()),
                            )
                            .child(appearance_selector),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(tokens.on_surface_variant)
                                    .child(rust_i18n::t!("settings.language_label").into_owned()),
                            )
                            .child(locale_selector),
                    ),
            )
            .child(
                SectionCard::new()
                    .title(rust_i18n::t!("settings.network.title").into_owned())
                    .subtitle(rust_i18n::t!("settings.network.subtitle").into_owned())
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(px(12.0))
                            .child(MetricTile::new(
                                rust_i18n::t!("common.mixed_port").into_owned(),
                                syncable.mixed_port.to_string(),
                            ))
                            .child(MetricTile::new(
                                rust_i18n::t!("settings.network.allow_lan").into_owned(),
                                if syncable.allow_lan {
                                    enabled_label.clone()
                                } else {
                                    disabled_label.clone()
                                },
                            ))
                            .child(MetricTile::new(
                                rust_i18n::t!("settings.network.ipv6").into_owned(),
                                if syncable.ipv6 {
                                    enabled_label.clone()
                                } else {
                                    disabled_label.clone()
                                },
                            ))
                            .child(MetricTile::new(
                                rust_i18n::t!("settings.network.log_level").into_owned(),
                                syncable.log_level.clone(),
                            )),
                    )
                    .child(MetricTile::new(
                        rust_i18n::t!("settings.network.bypass").into_owned(),
                        bypass_summary,
                    )),
            )
            .child(
                SectionCard::new()
                    .title(rust_i18n::t!("settings.machine.title").into_owned())
                    .subtitle(rust_i18n::t!("settings.machine.subtitle").into_owned())
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(px(12.0))
                            .child(MetricTile::new(
                                rust_i18n::t!("settings.machine.service_mode").into_owned(),
                                if local.service_mode_enabled {
                                    enabled_label.clone()
                                } else {
                                    rust_i18n::t!("settings.machine.service_disabled").into_owned()
                                },
                            ))
                            .child(MetricTile::new(
                                rust_i18n::t!("settings.machine.tun_interface").into_owned(),
                                local.tun_interface_name.clone().unwrap_or_else(|| {
                                    rust_i18n::t!("settings.machine.tun_auto").into_owned()
                                }),
                            ))
                            .child(MetricTile::new(
                                rust_i18n::t!("settings.machine.auto_launch").into_owned(),
                                if local.auto_launch {
                                    enabled_label
                                } else {
                                    disabled_label
                                },
                            )),
                    ),
            )
    }
}
