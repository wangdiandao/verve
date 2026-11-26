//! Settings feature slice: responsive category navigation and inline forms for
//! network takeover, TUN core parameters, privileged service mode, and appearance/general settings.

mod general;
mod network;

use gpui::{
    App, AppContext, Context, Entity, Focusable, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, WindowAppearance, div, prelude::FluentBuilder, px,
};
use gpui_material::{
    ButtonGroup, ButtonGroupItem, ButtonGroupSelection, TextAreaState, TextFieldEvent,
    TextFieldState,
};
use gpui_material_symbols::Symbol;
use verve_core::config::TunConfig;
use verve_core::{DomainKind, Revision, ServiceControlAction, SettingsSlice};
use verve_ui_kit::{
    AppearanceMode, DEFAULT_SEED_ARGB, apply_theme, build_material_theme, format_hex_seed_color,
    parse_hex_seed_color, theme_tokens,
};

use crate::bridge::{CommandDispatcher, DomainChanged, UiStore};
use crate::shell::nav::symbol_icon;
use crate::shell::{ConfirmDialogSpec, ShellHandle};

/// Minimum valid TUN virtual interface MTU (IPv6 floor).
pub const MIN_TUN_MTU: u16 = 1280;

/// Viewport width breakpoint below which the left category rail collapses into a horizontal bar.
pub const NARROW_VIEWPORT_BREAKPOINT_PX: f32 = 820.0;

/// Top-level settings categories exposed in the `0.0.3` Settings feature slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SettingsCategory {
    /// Inbound proxy port, system proxy bypass, TUN core parameters, and privileged service mode.
    #[default]
    Network,
    /// Theme mode, dynamic seed accent color, UI language, and OS login auto-launch.
    General,
}

impl SettingsCategory {
    /// All visible settings categories in display order.
    pub const ALL: [Self; 2] = [Self::Network, Self::General];

    /// Stable string identifier for element keys and segmented controls.
    pub const fn id_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::General => "general",
        }
    }

    /// Parses a [`SettingsCategory`] from its stable string identifier.
    pub fn from_id_str(id: &str) -> Option<Self> {
        match id {
            "network" => Some(Self::Network),
            "general" => Some(Self::General),
            _ => None,
        }
    }

    /// Localized display label for the active `rust-i18n` locale.
    pub fn localized_label(self) -> String {
        match self {
            Self::Network => rust_i18n::t!("settings.category.network").into_owned(),
            Self::General => rust_i18n::t!("settings.category.general").into_owned(),
        }
    }

    /// Material Symbol icon paired with this category.
    pub const fn symbol(self) -> Symbol {
        match self {
            Self::Network => Symbol::ROUTER,
            Self::General => Symbol::PALETTE,
        }
    }
}

/// Pure form draft values projected from [`SettingsSlice`] for deterministic input synchronization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsFormSnapshot {
    /// Formatted `mixed-port` text value.
    pub mixed_port: String,
    /// Formatted comma-separated system proxy bypass entries.
    pub bypass: String,
    /// Formatted TUN MTU text value.
    pub tun_mtu: String,
    /// Formatted comma-separated TUN DNS hijack entries.
    pub tun_dns_hijack: String,
    /// Formatted comma-separated TUN excluded CIDR routes.
    pub tun_exclude_routes: String,
    /// Formatted local TUN interface override (empty string for automatic naming).
    pub tun_interface: String,
    /// Formatted uppercase `#RRGGBB` dynamic color seed.
    pub seed_hex: String,
}

impl SettingsFormSnapshot {
    /// Projects a [`SettingsSlice`] into formatted form text values.
    pub fn from_slice(settings: &SettingsSlice) -> Self {
        let syncable = &settings.syncable;
        let local = &settings.local;
        let active_seed = syncable.theme_seed_color.unwrap_or(DEFAULT_SEED_ARGB);

        Self {
            mixed_port: syncable.mixed_port.to_string(),
            bypass: format_list_entries(&syncable.system_proxy_bypass),
            tun_mtu: syncable.tun.mtu.to_string(),
            tun_dns_hijack: format_list_entries(&syncable.tun.dns_hijack),
            tun_exclude_routes: format_list_entries(&syncable.tun.route_exclude_address),
            tun_interface: local.tun_interface_name.clone().unwrap_or_default(),
            seed_hex: format_hex_seed_color(active_seed),
        }
    }
}

/// Splits comma- or newline-separated list entries, trimming whitespace and dropping blanks.
pub fn parse_list_entries(raw: &str) -> Vec<String> {
    raw.split([',', '\n', '\r'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Joins list entries into a human-friendly comma-separated string.
pub fn format_list_entries(entries: &[String]) -> String {
    entries.join(", ")
}

/// Validates a `mixed-port` text field, accepting integers in `1..=65535`.
pub fn validate_mixed_port(raw: &str) -> Option<u16> {
    let parsed = raw.trim().parse::<u16>().ok()?;
    (parsed != 0).then_some(parsed)
}

/// Validates a TUN MTU text field, accepting integers in `1280..=65535`.
pub fn validate_tun_mtu(raw: &str) -> Option<u16> {
    let parsed = raw.trim().parse::<u16>().ok()?;
    (parsed >= MIN_TUN_MTU).then_some(parsed)
}

/// Validates and normalizes a TUN network stack identifier (`"system"`, `"gvisor"`, or `"mixed"`).
pub fn normalize_tun_stack(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "system" => Some("system"),
        "gvisor" => Some("gvisor"),
        "mixed" => Some("mixed"),
        _ => None,
    }
}

/// Trims an optional input string and collapses blank strings to `None`.
pub fn normalize_optional_text(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Returns `true` when an input should be synced from backend state without clobbering active edits.
pub fn should_sync_field(
    is_dirty: bool,
    is_focused: bool,
    current_value: &str,
    target_value: &str,
) -> bool {
    !is_dirty && !is_focused && current_value != target_value
}

#[derive(Debug, Default)]
struct FieldDirtyFlags {
    mixed_port: bool,
    bypass: bool,
    tun_mtu: bool,
    tun_dns_hijack: bool,
    tun_exclude_routes: bool,
    tun_interface: bool,
    seed_hex: bool,
}

pub(super) struct SettingsFormInputs {
    pub(super) mixed_port: Entity<TextFieldState>,
    pub(super) bypass: Entity<TextAreaState>,
    pub(super) tun_mtu: Entity<TextFieldState>,
    pub(super) tun_dns_hijack: Entity<TextFieldState>,
    pub(super) tun_exclude_routes: Entity<TextFieldState>,
    pub(super) tun_interface: Entity<TextFieldState>,
    pub(super) custom_seed_hex: Entity<TextFieldState>,
    _subscriptions: Vec<Subscription>,
}

/// Vertical feature view projecting [`SettingsSlice`] and [`verve_core::RuntimeSlice`]
/// (`service_status`) with scoped inline forms.
pub struct SettingsView {
    store: Entity<UiStore>,
    commands: CommandDispatcher,
    shell: ShellHandle,
    active_category: SettingsCategory,
    scroll: ScrollHandle,
    inputs: Option<SettingsFormInputs>,
    dirty: FieldDirtyFlags,
    synced_settings_revision: Revision,
    synced_locale: String,
    mixed_port_error: Option<String>,
    tun_mtu_error: Option<String>,
    seed_hex_error: Option<String>,
    _store_sub: Subscription,
}

impl SettingsView {
    /// Creates a new [`SettingsView`] subscribed to `Settings` and `Runtime` slice updates.
    pub fn new(
        store: Entity<UiStore>,
        commands: CommandDispatcher,
        shell: ShellHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        let store_sub = cx.subscribe(&store, |_, _, ev: &DomainChanged, cx| {
            if matches!(ev.0, DomainKind::Settings | DomainKind::Runtime) {
                cx.notify();
            }
        });
        Self {
            store,
            commands,
            shell,
            active_category: SettingsCategory::Network,
            scroll: ScrollHandle::new(),
            inputs: None,
            dirty: FieldDirtyFlags::default(),
            synced_settings_revision: 0,
            synced_locale: String::new(),
            mixed_port_error: None,
            tun_mtu_error: None,
            seed_hex_error: None,
            _store_sub: store_sub,
        }
    }

    /// Returns the currently selected [`SettingsCategory`].
    pub const fn active_category(&self) -> SettingsCategory {
        self.active_category
    }

    /// Switches the active settings category and resets scroll offset.
    pub fn select_category(&mut self, category: SettingsCategory, cx: &mut Context<Self>) {
        if self.active_category == category {
            return;
        }
        self.active_category = category;
        self.scroll.set_offset(gpui::Point::default());
        cx.notify();
    }

    /// Returns the inline validation error for `mixed-port`, if any.
    pub fn mixed_port_error(&self) -> Option<&str> {
        self.mixed_port_error.as_deref()
    }

    /// Returns the inline validation error for TUN MTU, if any.
    pub fn tun_mtu_error(&self) -> Option<&str> {
        self.tun_mtu_error.as_deref()
    }

    /// Returns the inline validation error for the custom HEX seed color, if any.
    pub fn seed_hex_error(&self) -> Option<&str> {
        self.seed_hex_error.as_deref()
    }

    /// Updates the `mixed-port` draft input programmatically (primarily for headless tests).
    pub fn set_mixed_port_draft(
        &mut self,
        value: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_inputs(window, cx);
        if let Some(inputs) = &self.inputs {
            let value = value.into();
            inputs
                .mixed_port
                .update(cx, |state, cx| state.set_value(value, window, cx));
            self.dirty.mixed_port = true;
            self.mixed_port_error = None;
            cx.notify();
        }
    }

    /// Updates the system proxy bypass draft input programmatically.
    pub fn set_bypass_draft(
        &mut self,
        value: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_inputs(window, cx);
        if let Some(inputs) = &self.inputs {
            let value = value.into();
            inputs
                .bypass
                .update(cx, |state, cx| state.set_value(value, window, cx));
            self.dirty.bypass = true;
            cx.notify();
        }
    }

    /// Updates the TUN MTU draft input programmatically.
    pub fn set_tun_mtu_draft(
        &mut self,
        value: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_inputs(window, cx);
        if let Some(inputs) = &self.inputs {
            let value = value.into();
            inputs
                .tun_mtu
                .update(cx, |state, cx| state.set_value(value, window, cx));
            self.dirty.tun_mtu = true;
            self.tun_mtu_error = None;
            cx.notify();
        }
    }

    /// Updates the TUN DNS hijack draft input programmatically.
    pub fn set_tun_dns_hijack_draft(
        &mut self,
        value: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_inputs(window, cx);
        if let Some(inputs) = &self.inputs {
            let value = value.into();
            inputs
                .tun_dns_hijack
                .update(cx, |state, cx| state.set_value(value, window, cx));
            self.dirty.tun_dns_hijack = true;
            cx.notify();
        }
    }

    /// Updates the TUN exclude routes draft input programmatically.
    pub fn set_tun_exclude_routes_draft(
        &mut self,
        value: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_inputs(window, cx);
        if let Some(inputs) = &self.inputs {
            let value = value.into();
            inputs
                .tun_exclude_routes
                .update(cx, |state, cx| state.set_value(value, window, cx));
            self.dirty.tun_exclude_routes = true;
            cx.notify();
        }
    }

    /// Updates the local TUN interface name draft input programmatically.
    pub fn set_tun_interface_draft(
        &mut self,
        value: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_inputs(window, cx);
        if let Some(inputs) = &self.inputs {
            let value = value.into();
            inputs
                .tun_interface
                .update(cx, |state, cx| state.set_value(value, window, cx));
            self.dirty.tun_interface = true;
            cx.notify();
        }
    }

    /// Updates the custom HEX seed color draft input programmatically.
    pub fn set_custom_seed_hex_draft(
        &mut self,
        value: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_inputs(window, cx);
        if let Some(inputs) = &self.inputs {
            let value = value.into();
            inputs
                .custom_seed_hex
                .update(cx, |state, cx| state.set_value(value, window, cx));
            self.dirty.seed_hex = true;
            self.seed_hex_error = None;
            cx.notify();
        }
    }

    /// Validates inbound port & bypass inputs and dispatches [`CommandDispatcher::update_network_settings`].
    pub fn apply_network_form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.ensure_inputs(window, cx);
        let Some(inputs) = &self.inputs else {
            return false;
        };

        let port_raw = inputs.mixed_port.read(cx).value();
        let Some(port) = validate_mixed_port(port_raw.as_ref()) else {
            self.mixed_port_error =
                Some(rust_i18n::t!("settings.network.mixed_port_invalid").into_owned());
            cx.notify();
            return false;
        };

        let bypass_raw = inputs.bypass.read(cx).value();
        let bypass = parse_list_entries(bypass_raw.as_ref());
        let formatted_bypass = format_list_entries(&bypass);

        let syncable = self.store.read(cx).settings().syncable.clone();
        self.mixed_port_error = None;
        self.dirty.mixed_port = false;
        self.dirty.bypass = false;

        inputs.mixed_port.update(cx, |state, cx| {
            state.set_value(port.to_string(), window, cx)
        });
        inputs.bypass.update(cx, |state, cx| {
            state.set_value(formatted_bypass, window, cx)
        });

        self.commands
            .update_network_settings(port, syncable.allow_lan, syncable.ipv6, bypass);
        self.commands
            .notify_info(rust_i18n::t!("settings.saved_toast").into_owned());
        cx.notify();
        true
    }

    /// Resets the system proxy bypass list to empty (system default) and persists immediately.
    pub fn reset_bypass(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_inputs(window, cx);
        if let Some(inputs) = &self.inputs {
            inputs
                .bypass
                .update(cx, |state, cx| state.set_value("", window, cx));
        }
        self.dirty.bypass = false;

        let syncable = self.store.read(cx).settings().syncable.clone();
        self.commands.update_network_settings(
            syncable.mixed_port,
            syncable.allow_lan,
            syncable.ipv6,
            Vec::new(),
        );
        self.commands
            .notify_info(rust_i18n::t!("settings.saved_toast").into_owned());
        cx.notify();
    }

    /// Validates TUN form fields and dispatches [`CommandDispatcher::update_tun_settings`].
    pub fn apply_tun_form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.ensure_inputs(window, cx);
        let Some(inputs) = &self.inputs else {
            return false;
        };

        let mtu_raw = inputs.tun_mtu.read(cx).value();
        let Some(mtu) = validate_tun_mtu(mtu_raw.as_ref()) else {
            self.tun_mtu_error = Some(rust_i18n::t!("settings.tun.mtu_invalid").into_owned());
            cx.notify();
            return false;
        };

        let dns_hijack = parse_list_entries(inputs.tun_dns_hijack.read(cx).value().as_ref());
        let route_exclude_address =
            parse_list_entries(inputs.tun_exclude_routes.read(cx).value().as_ref());
        let tun_interface_name =
            normalize_optional_text(inputs.tun_interface.read(cx).value().as_ref());

        let current_tun = self.store.read(cx).settings().syncable.tun.clone();
        let stack = normalize_tun_stack(&current_tun.stack)
            .unwrap_or("mixed")
            .to_owned();

        let next_tun = TunConfig {
            stack,
            auto_route: current_tun.auto_route,
            strict_route: current_tun.strict_route,
            dns_hijack: dns_hijack.clone(),
            mtu,
            route_exclude_address: route_exclude_address.clone(),
        };

        self.tun_mtu_error = None;
        self.dirty.tun_mtu = false;
        self.dirty.tun_dns_hijack = false;
        self.dirty.tun_exclude_routes = false;
        self.dirty.tun_interface = false;

        inputs
            .tun_mtu
            .update(cx, |state, cx| state.set_value(mtu.to_string(), window, cx));
        inputs.tun_dns_hijack.update(cx, |state, cx| {
            state.set_value(format_list_entries(&dns_hijack), window, cx)
        });
        inputs.tun_exclude_routes.update(cx, |state, cx| {
            state.set_value(format_list_entries(&route_exclude_address), window, cx)
        });
        inputs.tun_interface.update(cx, |state, cx| {
            state.set_value(tun_interface_name.clone().unwrap_or_default(), window, cx)
        });

        self.commands
            .update_tun_settings(next_tun, tun_interface_name);
        self.commands
            .notify_info(rust_i18n::t!("settings.saved_toast").into_owned());
        cx.notify();
        true
    }

    /// Populates [`TunConfig::safe_defaults()`], clears interface override, and persists immediately.
    pub fn apply_tun_safe_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_inputs(window, cx);
        let defaults = TunConfig::safe_defaults();

        if let Some(inputs) = &self.inputs {
            inputs.tun_mtu.update(cx, |state, cx| {
                state.set_value(defaults.mtu.to_string(), window, cx)
            });
            inputs.tun_dns_hijack.update(cx, |state, cx| {
                state.set_value(format_list_entries(&defaults.dns_hijack), window, cx)
            });
            inputs.tun_exclude_routes.update(cx, |state, cx| {
                state.set_value(
                    format_list_entries(&defaults.route_exclude_address),
                    window,
                    cx,
                )
            });
            inputs
                .tun_interface
                .update(cx, |state, cx| state.set_value("", window, cx));
        }

        self.tun_mtu_error = None;
        self.dirty.tun_mtu = false;
        self.dirty.tun_dns_hijack = false;
        self.dirty.tun_exclude_routes = false;
        self.dirty.tun_interface = false;

        self.commands.update_tun_settings(defaults, None);
        self.commands
            .notify_info(rust_i18n::t!("settings.saved_toast").into_owned());
        cx.notify();
    }

    /// Opens the destructive confirmation dialog before uninstalling `verve-service`.
    pub fn request_service_uninstall(&self, window: &mut Window, cx: &mut App) {
        let commands = self.commands.clone();
        let spec = ConfirmDialogSpec::new(
            rust_i18n::t!("settings.service.uninstall_confirm_title").into_owned(),
            rust_i18n::t!("settings.service.uninstall_confirm_desc").into_owned(),
            rust_i18n::t!("settings.service.uninstall").into_owned(),
            move |_, _| {
                commands.service_action(ServiceControlAction::Uninstall);
            },
        )
        .destructive(true);
        self.shell.open_confirm(spec, window, cx);
    }

    /// Applies an [`AppearanceMode`] live and persists it through [`CommandDispatcher`].
    pub fn apply_theme_mode(&self, mode: AppearanceMode, cx: &mut App) {
        let syncable = self.store.read(cx).settings().syncable.clone();
        let active_seed = syncable.theme_seed_color.or(Some(DEFAULT_SEED_ARGB));
        let system_dark = matches!(
            cx.window_appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        apply_theme(build_material_theme(mode, active_seed, system_dark), cx);
        self.commands.set_theme_mode(mode.as_config_str());
    }

    /// Applies an optional ARGB dynamic seed color live, syncs the HEX input, and persists it.
    pub fn apply_seed_color(
        &mut self,
        seed_argb: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_inputs(window, cx);
        let syncable = self.store.read(cx).settings().syncable.clone();
        let mode =
            AppearanceMode::from_config_str(&syncable.theme_mode).unwrap_or(AppearanceMode::System);
        let system_dark = matches!(
            cx.window_appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let effective_seed = seed_argb.unwrap_or(DEFAULT_SEED_ARGB);

        apply_theme(
            build_material_theme(mode, Some(effective_seed), system_dark),
            cx,
        );

        if let Some(inputs) = &self.inputs {
            inputs.custom_seed_hex.update(cx, |state, cx| {
                state.set_value(format_hex_seed_color(effective_seed), window, cx)
            });
        }
        self.seed_hex_error = None;
        self.dirty.seed_hex = false;

        self.commands.set_theme_seed_color(seed_argb);
        self.commands
            .notify_info(rust_i18n::t!("settings.saved_toast").into_owned());
        cx.notify();
    }

    /// Validates the custom HEX seed color input, applies it live, and persists it.
    pub fn apply_custom_seed_hex(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.ensure_inputs(window, cx);
        let Some(inputs) = &self.inputs else {
            return false;
        };

        let raw = inputs.custom_seed_hex.read(cx).value();
        let Some(argb) = parse_hex_seed_color(raw.as_ref()) else {
            self.seed_hex_error =
                Some(rust_i18n::t!("settings.accent.custom_hex_invalid").into_owned());
            cx.notify();
            return false;
        };

        self.apply_seed_color(Some(argb), window, cx);
        true
    }

    pub(super) fn ensure_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = self.store.read(cx).settings().clone();
        let draft = SettingsFormSnapshot::from_slice(&settings);
        let current_locale = rust_i18n::locale().to_string();

        if self.inputs.is_none() {
            let mixed_port = cx.new(|cx| {
                TextFieldState::new(window, cx)
                    .default_value(draft.mixed_port.clone())
                    .placeholder(
                        rust_i18n::t!("settings.network.mixed_port_placeholder").into_owned(),
                    )
            });
            let bypass = cx.new(|cx| {
                TextAreaState::new(window, cx)
                    .auto_grow(2, 5)
                    .default_value(draft.bypass.clone())
                    .placeholder(rust_i18n::t!("settings.network.bypass_placeholder").into_owned())
            });
            let tun_mtu = cx.new(|cx| {
                TextFieldState::new(window, cx)
                    .default_value(draft.tun_mtu.clone())
                    .placeholder(rust_i18n::t!("settings.tun.mtu_placeholder").into_owned())
            });
            let tun_dns_hijack = cx.new(|cx| {
                TextFieldState::new(window, cx)
                    .default_value(draft.tun_dns_hijack.clone())
                    .placeholder(rust_i18n::t!("settings.tun.dns_hijack_placeholder").into_owned())
            });
            let tun_exclude_routes = cx.new(|cx| {
                TextFieldState::new(window, cx)
                    .default_value(draft.tun_exclude_routes.clone())
                    .placeholder(
                        rust_i18n::t!("settings.tun.exclude_routes_placeholder").into_owned(),
                    )
            });
            let tun_interface = cx.new(|cx| {
                TextFieldState::new(window, cx)
                    .default_value(draft.tun_interface.clone())
                    .placeholder(rust_i18n::t!("settings.machine.tun_auto").into_owned())
            });
            let custom_seed_hex = cx.new(|cx| {
                TextFieldState::new(window, cx)
                    .default_value(draft.seed_hex.clone())
                    .placeholder(
                        rust_i18n::t!("settings.accent.custom_hex_placeholder").into_owned(),
                    )
            });

            let subscriptions = vec![
                cx.subscribe(&mixed_port, |this, _, ev: &TextFieldEvent, cx| {
                    if matches!(ev, TextFieldEvent::Change) {
                        this.dirty.mixed_port = true;
                        this.mixed_port_error = None;
                        cx.notify();
                    }
                }),
                cx.subscribe(&bypass, |this, _, ev: &TextFieldEvent, cx| {
                    if matches!(ev, TextFieldEvent::Change) {
                        this.dirty.bypass = true;
                        cx.notify();
                    }
                }),
                cx.subscribe(&tun_mtu, |this, _, ev: &TextFieldEvent, cx| {
                    if matches!(ev, TextFieldEvent::Change) {
                        this.dirty.tun_mtu = true;
                        this.tun_mtu_error = None;
                        cx.notify();
                    }
                }),
                cx.subscribe(&tun_dns_hijack, |this, _, ev: &TextFieldEvent, cx| {
                    if matches!(ev, TextFieldEvent::Change) {
                        this.dirty.tun_dns_hijack = true;
                        cx.notify();
                    }
                }),
                cx.subscribe(&tun_exclude_routes, |this, _, ev: &TextFieldEvent, cx| {
                    if matches!(ev, TextFieldEvent::Change) {
                        this.dirty.tun_exclude_routes = true;
                        cx.notify();
                    }
                }),
                cx.subscribe(&tun_interface, |this, _, ev: &TextFieldEvent, cx| {
                    if matches!(ev, TextFieldEvent::Change) {
                        this.dirty.tun_interface = true;
                        cx.notify();
                    }
                }),
                cx.subscribe(&custom_seed_hex, |this, _, ev: &TextFieldEvent, cx| {
                    if matches!(ev, TextFieldEvent::Change) {
                        this.dirty.seed_hex = true;
                        this.seed_hex_error = None;
                        cx.notify();
                    }
                }),
            ];

            self.inputs = Some(SettingsFormInputs {
                mixed_port,
                bypass,
                tun_mtu,
                tun_dns_hijack,
                tun_exclude_routes,
                tun_interface,
                custom_seed_hex,
                _subscriptions: subscriptions,
            });
            self.synced_settings_revision = settings.revision;
            self.synced_locale = current_locale;
            return;
        }

        if let Some(inputs) = &self.inputs {
            if settings.revision != self.synced_settings_revision {
                sync_single_input(
                    &inputs.mixed_port,
                    self.dirty.mixed_port,
                    &draft.mixed_port,
                    window,
                    cx,
                );
                sync_multiline_input(&inputs.bypass, self.dirty.bypass, &draft.bypass, window, cx);
                sync_single_input(
                    &inputs.tun_mtu,
                    self.dirty.tun_mtu,
                    &draft.tun_mtu,
                    window,
                    cx,
                );
                sync_single_input(
                    &inputs.tun_dns_hijack,
                    self.dirty.tun_dns_hijack,
                    &draft.tun_dns_hijack,
                    window,
                    cx,
                );
                sync_single_input(
                    &inputs.tun_exclude_routes,
                    self.dirty.tun_exclude_routes,
                    &draft.tun_exclude_routes,
                    window,
                    cx,
                );
                sync_single_input(
                    &inputs.tun_interface,
                    self.dirty.tun_interface,
                    &draft.tun_interface,
                    window,
                    cx,
                );
                sync_single_input(
                    &inputs.custom_seed_hex,
                    self.dirty.seed_hex,
                    &draft.seed_hex,
                    window,
                    cx,
                );
                self.synced_settings_revision = settings.revision;
            }

            if current_locale != self.synced_locale {
                inputs.tun_interface.update(cx, |state, cx| {
                    state.set_placeholder(
                        rust_i18n::t!("settings.machine.tun_auto").into_owned(),
                        window,
                        cx,
                    );
                });
                self.synced_locale = current_locale;
            }
        }
    }

    fn render_category_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let active = self.active_category;

        let mut rail = div()
            .id("settings-category-rail")
            .w(px(200.0))
            .flex_none()
            .h_full()
            .p(px(16.0))
            .border_r_1()
            .border_color(tokens.outline_variant)
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .px(px(8.0))
                    .pb(px(10.0))
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(tokens.on_surface)
                    .child(rust_i18n::t!("settings.title").into_owned()),
            );

        for category in SettingsCategory::ALL {
            let is_selected = category == active;
            let weak = cx.weak_entity();
            let bg = if is_selected {
                tokens.secondary_container
            } else {
                gpui::transparent_black()
            };
            let fg = if is_selected {
                tokens.on_secondary_container
            } else {
                tokens.on_surface_variant
            };
            let hover_bg = if is_selected {
                tokens.secondary_container
            } else {
                tokens.surface_container_high
            };

            rail = rail.child(
                div()
                    .id(SharedString::from(format!(
                        "settings-cat-{}",
                        category.id_str()
                    )))
                    .w_full()
                    .px(px(12.0))
                    .py(px(10.0))
                    .rounded(px(12.0))
                    .bg(bg)
                    .hover(move |style| style.bg(hover_bg))
                    .cursor_pointer()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .text_sm()
                    .text_color(fg)
                    .when(is_selected, |el| el.font_weight(FontWeight::SEMIBOLD))
                    .when(!is_selected, |el| el.font_weight(FontWeight::MEDIUM))
                    .child(div().size(px(18.0)).child(symbol_icon(category.symbol())))
                    .child(category.localized_label())
                    .on_click(move |_, _, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            this.select_category(category, cx);
                        });
                    }),
            );
        }

        rail
    }

    fn render_narrow_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let weak = cx.weak_entity();
        let active_id = self.active_category.id_str();

        let tabs = ButtonGroup::connected(
            "settings-category-tabs",
            rust_i18n::t!("settings.category_nav_label").into_owned(),
            SettingsCategory::ALL
                .iter()
                .map(|cat| ButtonGroupItem::new(cat.id_str(), cat.localized_label()))
                .collect::<Vec<_>>(),
        )
        .selection(ButtonGroupSelection::SingleRequired, [active_id.into()])
        .on_change(move |selected, _, cx| {
            if let Some(first) = selected.first()
                && let Some(category) = SettingsCategory::from_id_str(first.as_ref())
            {
                let _ = weak.update(cx, |this, cx| {
                    this.select_category(category, cx);
                });
            }
        });

        div()
            .id("settings-narrow-header")
            .w_full()
            .px(px(16.0))
            .pt(px(16.0))
            .pb(px(12.0))
            .border_b_1()
            .border_color(tokens.outline_variant)
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
                    .child(rust_i18n::t!("settings.title").into_owned()),
            )
            .child(tabs)
    }
}

fn sync_single_input(
    entity: &Entity<TextFieldState>,
    is_dirty: bool,
    target: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let (is_focused, current_value) = {
        let state = entity.read(cx);
        (state.focus_handle(cx).is_focused(window), state.value())
    };
    if should_sync_field(is_dirty, is_focused, current_value.as_ref(), target) {
        let next = target.to_owned();
        entity.update(cx, |state, cx| state.set_value(next, window, cx));
    }
}

fn sync_multiline_input(
    entity: &Entity<TextAreaState>,
    is_dirty: bool,
    target: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let (is_focused, current_value) = {
        let state = entity.read(cx);
        (state.focus_handle(cx).is_focused(window), state.value())
    };
    if should_sync_field(is_dirty, is_focused, current_value.as_ref(), target) {
        let next = target.to_owned();
        entity.update(cx, |state, cx| state.set_value(next, window, cx));
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_inputs(window, cx);

        let viewport_width = f32::from(window.viewport_size().width);
        let is_narrow = viewport_width < NARROW_VIEWPORT_BREAKPOINT_PX;

        let form_body = match self.active_category {
            SettingsCategory::Network => self.render_network_category(cx).into_any_element(),
            SettingsCategory::General => self.render_general_category(cx).into_any_element(),
        };

        let content_pane = div()
            .id("settings-content-pane")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p(px(if is_narrow { 16.0 } else { 20.0 }))
            .child(
                div()
                    .w_full()
                    .max_w(px(860.0))
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .child(form_body),
            );

        if is_narrow {
            div()
                .id("settings-page")
                .size_full()
                .flex()
                .flex_col()
                .child(self.render_narrow_header(cx))
                .child(content_pane)
        } else {
            div()
                .id("settings-page")
                .size_full()
                .flex()
                .flex_row()
                .child(self.render_category_rail(cx))
                .child(content_pane)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comma_and_newline_list_entries() {
        assert_eq!(
            parse_list_entries("localhost, 127.0.0.1\n::1,\r\n  *.local ,  "),
            vec!["localhost", "127.0.0.1", "::1", "*.local"]
        );
        assert!(parse_list_entries("  , \n \r\n ").is_empty());
        assert_eq!(
            format_list_entries(&["any:53".into(), "tcp://any:53".into()]),
            "any:53, tcp://any:53"
        );
    }

    #[test]
    fn validates_mixed_port_and_tun_mtu_ranges() {
        assert_eq!(validate_mixed_port("7897"), Some(7897));
        assert_eq!(validate_mixed_port(" 1 "), Some(1));
        assert_eq!(validate_mixed_port("65535"), Some(65535));
        assert_eq!(validate_mixed_port("0"), None);
        assert_eq!(validate_mixed_port("65536"), None);
        assert_eq!(validate_mixed_port("-1"), None);
        assert_eq!(validate_mixed_port("abc"), None);
        assert_eq!(validate_mixed_port(""), None);

        assert_eq!(validate_tun_mtu("1280"), Some(1280));
        assert_eq!(validate_tun_mtu(" 9000 "), Some(9000));
        assert_eq!(validate_tun_mtu("65535"), Some(65535));
        assert_eq!(validate_tun_mtu("1279"), None);
        assert_eq!(validate_tun_mtu("0"), None);
        assert_eq!(validate_tun_mtu("70000"), None);
        assert_eq!(validate_tun_mtu("invalid"), None);
    }

    #[test]
    fn normalizes_tun_stack_and_optional_text() {
        assert_eq!(normalize_tun_stack("System"), Some("system"));
        assert_eq!(normalize_tun_stack(" gvisor "), Some("gvisor"));
        assert_eq!(normalize_tun_stack("MIXED"), Some("mixed"));
        assert_eq!(normalize_tun_stack("lwip"), None);

        assert_eq!(normalize_optional_text(""), None);
        assert_eq!(normalize_optional_text("   \t "), None);
        assert_eq!(normalize_optional_text(" utun9 "), Some("utun9".into()));
    }

    #[test]
    fn projects_settings_slice_and_guards_draft_sync() {
        let mut slice = SettingsSlice::default();
        slice.syncable.mixed_port = 10809;
        slice.syncable.system_proxy_bypass = vec!["localhost".into(), "10.0.0.0/8".into()];
        slice.syncable.tun.mtu = 1500;
        slice.syncable.tun.dns_hijack = vec!["any:53".into(), "tcp://any:53".into()];
        slice.syncable.tun.route_exclude_address = vec!["192.168.0.0/16".into()];
        slice.syncable.theme_seed_color = Some(0xFF4F_5B92);
        slice.local.tun_interface_name = Some("verve-tun".into());

        let snapshot = SettingsFormSnapshot::from_slice(&slice);
        assert_eq!(snapshot.mixed_port, "10809");
        assert_eq!(snapshot.bypass, "localhost, 10.0.0.0/8");
        assert_eq!(snapshot.tun_mtu, "1500");
        assert_eq!(snapshot.tun_dns_hijack, "any:53, tcp://any:53");
        assert_eq!(snapshot.tun_exclude_routes, "192.168.0.0/16");
        assert_eq!(snapshot.tun_interface, "verve-tun");
        assert_eq!(snapshot.seed_hex, "#4F5B92");

        // Clean, unfocused, divergent -> sync
        assert!(should_sync_field(false, false, "7897", "10809"));
        // Dirty or focused or already matching -> do not clobber
        assert!(!should_sync_field(true, false, "7897", "10809"));
        assert!(!should_sync_field(false, true, "7897", "10809"));
        assert!(!should_sync_field(false, false, "10809", "10809"));
    }

    #[test]
    fn category_roundtrips_stable_ids() {
        for category in SettingsCategory::ALL {
            assert_eq!(
                SettingsCategory::from_id_str(category.id_str()),
                Some(category)
            );
        }
        assert_eq!(SettingsCategory::from_id_str("unknown"), None);
    }
}
