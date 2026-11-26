//! General & Appearance settings category: theme mode, Material 3 dynamic accent seed color
//! presets & custom HEX input, UI language selector, and OS login auto-launch.

use gpui::{
    Context, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::FluentBuilder, px,
};
use gpui_material::foundation::color as argb_to_hsla;
use gpui_material::theme::Argb;
use gpui_material::{
    Button, ButtonGroup, ButtonGroupItem, ButtonGroupSelection, ChipGroup, ChipGroupItem,
    ChipGroupSelection, SwitchTile, TextField,
};
use gpui_material_symbols::Symbol;
use verve_ui_kit::{
    AppearanceMode, DEFAULT_SEED_ARGB, SEED_COLOR_PRESETS, ThemeSeedPreset, theme_tokens,
};

use super::SettingsView;
use crate::shell::nav::symbol_icon;

/// Returns the localized display label for a curated [`ThemeSeedPreset`].
pub(super) fn preset_localized_label(preset: &ThemeSeedPreset) -> String {
    match preset.id {
        "deep_teal" => rust_i18n::t!("settings.accent.presets.deep_teal").into_owned(),
        "ocean_blue" => rust_i18n::t!("settings.accent.presets.ocean_blue").into_owned(),
        "indigo" => rust_i18n::t!("settings.accent.presets.indigo").into_owned(),
        "forest_green" => rust_i18n::t!("settings.accent.presets.forest_green").into_owned(),
        "terracotta" => rust_i18n::t!("settings.accent.presets.terracotta").into_owned(),
        "plum" => rust_i18n::t!("settings.accent.presets.plum").into_owned(),
        "amber" => rust_i18n::t!("settings.accent.presets.amber").into_owned(),
        other => other.to_owned(),
    }
}

impl SettingsView {
    pub(super) fn render_general_category(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("settings-category-general")
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(self.render_appearance_card(cx))
            .child(self.render_language_startup_card(cx))
    }

    fn render_appearance_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let settings = self.store.read(cx).settings().clone();
        let syncable = &settings.syncable;
        let inputs = self
            .inputs
            .as_ref()
            .expect("settings inputs must be initialized before render");

        let current_mode =
            AppearanceMode::from_config_str(&syncable.theme_mode).unwrap_or(AppearanceMode::System);
        let active_seed = syncable.theme_seed_color.unwrap_or(DEFAULT_SEED_ARGB);

        let mode_view = cx.weak_entity();
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
                let _ = mode_view.update(cx, |this, cx| {
                    this.apply_theme_mode(mode, cx);
                });
            }
        });

        let active_preset_id = SEED_COLOR_PRESETS
            .iter()
            .find(|preset| preset.argb == active_seed)
            .map(|preset| preset.id);

        let swatch_items: Vec<ChipGroupItem> = SEED_COLOR_PRESETS
            .iter()
            .map(|preset| {
                let is_selected = preset.argb == active_seed;
                let swatch_color = argb_to_hsla(Argb::from_u32(preset.argb));
                let swatch_dot = div()
                    .size(px(18.0))
                    .rounded_full()
                    .bg(swatch_color)
                    .border_1()
                    .border_color(if is_selected {
                        tokens.on_secondary_container.opacity(0.25)
                    } else {
                        tokens.outline_variant
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(is_selected, |el| {
                        el.child(
                            symbol_icon(Symbol::CHECK)
                                .size(px(12.0))
                                .color_override(gpui::white()),
                        )
                    });

                ChipGroupItem::new(preset.id, preset_localized_label(preset))
                    .leading_icon(swatch_dot)
            })
            .collect();

        let swatch_view = cx.weak_entity();
        let swatch_row = ChipGroup::new(
            "settings-seed-swatches",
            rust_i18n::t!("settings.accent.label").into_owned(),
            swatch_items,
        )
        .selection(
            ChipGroupSelection::SingleRequired,
            active_preset_id.into_iter().map(SharedString::from),
        )
        .on_change(move |selected, window, cx| {
            if let Some(first) = selected.first()
                && let Some(preset) = SEED_COLOR_PRESETS
                    .iter()
                    .find(|preset| preset.id == first.as_ref())
            {
                let preset_argb = preset.argb;
                let _ = swatch_view.update(cx, |this, cx| {
                    this.apply_seed_color(Some(preset_argb), window, cx);
                });
            }
        });

        let mut hex_field = TextField::outlined(
            "settings-custom-seed-hex",
            rust_i18n::t!("settings.accent.custom_hex_label").into_owned(),
            &inputs.custom_seed_hex,
        );
        hex_field = if let Some(err) = self.seed_hex_error.as_ref() {
            hex_field.error(err.clone())
        } else {
            hex_field.supporting_text(
                rust_i18n::t!("settings.accent.custom_hex_supporting").into_owned(),
            )
        };

        let apply_hex_view = cx.weak_entity();
        let apply_hex_btn = Button::tonal(
            "settings-apply-seed-hex",
            rust_i18n::t!("settings.accent.apply_color").into_owned(),
        )
        .leading_icon(symbol_icon(Symbol::CHECK))
        .on_activate(move |window, cx| {
            let _ = apply_hex_view.update(cx, |this, cx| {
                this.apply_custom_seed_hex(window, cx);
            });
        });

        let reset_seed_view = cx.weak_entity();
        let reset_seed_btn = Button::outlined(
            "settings-reset-seed-color",
            rust_i18n::t!("settings.accent.reset_default").into_owned(),
        )
        .leading_icon(symbol_icon(Symbol::RESTART_ALT))
        .on_activate(move |window, cx| {
            let _ = reset_seed_view.update(cx, |this, cx| {
                this.apply_seed_color(None, window, cx);
            });
        });

        verve_ui_kit::SectionCard::new()
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
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(tokens.on_surface_variant)
                                    .child(rust_i18n::t!("settings.accent.label").into_owned()),
                            )
                            .child(
                                div().text_xs().text_color(tokens.on_surface_variant).child(
                                    rust_i18n::t!("settings.accent.supporting").into_owned(),
                                ),
                            ),
                    )
                    .child(swatch_row),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_start()
                    .gap(px(10.0))
                    .child(div().w(px(240.0)).max_w_full().child(hex_field))
                    .child(
                        div()
                            .pt(px(6.0))
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .items_center()
                            .gap(px(8.0))
                            .child(apply_hex_btn)
                            .child(reset_seed_btn),
                    ),
            )
    }

    fn render_language_startup_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let settings = self.store.read(cx).settings().clone();
        let syncable = &settings.syncable;
        let local = &settings.local;

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

        let autolaunch_commands = self.commands.clone();
        let auto_launch_switch = SwitchTile::new(
            "settings-auto-launch",
            rust_i18n::t!("settings.machine.auto_launch").into_owned(),
            local.auto_launch,
        )
        .supporting(rust_i18n::t!("settings.general.auto_launch_supporting").into_owned())
        .on_change(move |enabled, _, _| {
            autolaunch_commands.set_auto_launch(enabled);
        });

        verve_ui_kit::SectionCard::new()
            .title(rust_i18n::t!("settings.general.title").into_owned())
            .subtitle(rust_i18n::t!("settings.general.subtitle").into_owned())
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
            )
            .child(auto_launch_switch)
    }
}
