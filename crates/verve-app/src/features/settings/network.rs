//! Network & Takeover settings category: inbound proxy listener, system proxy bypass,
//! TUN core configuration, and privileged `verve-service` supervision.

use gpui::{Context, FontWeight, InteractiveElement, IntoElement, ParentElement, Styled, div, px};
use gpui_material::{
    Banner, Button, ButtonGroup, ButtonGroupItem, ButtonGroupSelection, SemanticTone, SwitchTile,
    TextField, TextFieldVariant,
};
use gpui_material_symbols::Symbol;
use verve_core::{ServiceControlAction, ServiceStatus};
use verve_ui_kit::{HealthIndicator, KernelHealthState, SectionCard, theme_tokens};

use super::{SettingsView, normalize_tun_stack};
use crate::shell::nav::symbol_icon;

/// Returns the platform-appropriate i18n key for privileged TUN guidance.
pub(super) const fn platform_service_guidance_key() -> &'static str {
    if cfg!(target_os = "linux") {
        "settings.service.guidance_linux"
    } else {
        "settings.service.guidance_desktop"
    }
}

/// Resolves the [`KernelHealthState`] and localized pill label for a [`ServiceStatus`].
pub(super) fn service_status_badge(
    status: &ServiceStatus,
    service_mode_enabled: bool,
) -> (KernelHealthState, String) {
    match status {
        ServiceStatus::Available {
            running_pid: Some(pid),
        } => (
            KernelHealthState::Running,
            rust_i18n::t!("settings.service.status_available_pid", pid = pid).into_owned(),
        ),
        ServiceStatus::Available { running_pid: None } => (
            KernelHealthState::Running,
            rust_i18n::t!("settings.service.status_available").into_owned(),
        ),
        ServiceStatus::Unavailable { reason } => {
            let state = if service_mode_enabled && reason.is_some() {
                KernelHealthState::Fault
            } else {
                KernelHealthState::Stopped
            };
            (
                state,
                rust_i18n::t!("settings.service.status_unavailable").into_owned(),
            )
        }
        ServiceStatus::Unknown => (
            KernelHealthState::Stopped,
            rust_i18n::t!("settings.service.status_unknown").into_owned(),
        ),
    }
}

impl SettingsView {
    pub(super) fn render_network_category(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("settings-category-network")
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(self.render_inbound_card(cx))
            .child(self.render_tun_card(cx))
            .child(self.render_service_card(cx))
    }

    fn render_inbound_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.store.read(cx).settings().clone();
        let syncable = &settings.syncable;
        let inputs = self
            .inputs
            .as_ref()
            .expect("settings inputs must be initialized before render");

        let mut port_field = TextField::outlined(
            "settings-mixed-port",
            rust_i18n::t!("common.mixed_port").into_owned(),
            &inputs.mixed_port,
        );
        port_field = if let Some(err) = self.mixed_port_error.as_ref() {
            port_field.error(err.clone())
        } else {
            port_field.supporting_text(
                rust_i18n::t!("settings.network.mixed_port_supporting").into_owned(),
            )
        };

        let lan_commands = self.commands.clone();
        let lan_port = syncable.mixed_port;
        let lan_ipv6 = syncable.ipv6;
        let lan_bypass = syncable.system_proxy_bypass.clone();
        let allow_lan_switch = SwitchTile::new(
            "settings-allow-lan",
            rust_i18n::t!("settings.network.allow_lan").into_owned(),
            syncable.allow_lan,
        )
        .supporting(rust_i18n::t!("settings.network.allow_lan_supporting").into_owned())
        .on_change(move |enabled, _, _| {
            lan_commands.update_network_settings(lan_port, enabled, lan_ipv6, lan_bypass.clone());
        });

        let ipv6_commands = self.commands.clone();
        let ipv6_port = syncable.mixed_port;
        let ipv6_allow_lan = syncable.allow_lan;
        let ipv6_bypass = syncable.system_proxy_bypass.clone();
        let ipv6_switch = SwitchTile::new(
            "settings-ipv6",
            rust_i18n::t!("settings.network.ipv6").into_owned(),
            syncable.ipv6,
        )
        .supporting(rust_i18n::t!("settings.network.ipv6_supporting").into_owned())
        .on_change(move |enabled, _, _| {
            ipv6_commands.update_network_settings(
                ipv6_port,
                ipv6_allow_lan,
                enabled,
                ipv6_bypass.clone(),
            );
        });

        let bypass_field = TextField::multiline(
            "settings-system-proxy-bypass",
            rust_i18n::t!("settings.network.bypass").into_owned(),
            &inputs.bypass,
        )
        .variant(TextFieldVariant::Outlined)
        .supporting_text(rust_i18n::t!("settings.network.bypass_supporting").into_owned());

        let apply_view = cx.weak_entity();
        let apply_btn = Button::filled(
            "settings-network-apply",
            rust_i18n::t!("common.apply").into_owned(),
        )
        .leading_icon(symbol_icon(Symbol::CHECK))
        .on_activate(move |window, cx| {
            let _ = apply_view.update(cx, |this, cx| {
                this.apply_network_form(window, cx);
            });
        });

        let reset_view = cx.weak_entity();
        let reset_bypass_btn = Button::outlined(
            "settings-bypass-reset",
            rust_i18n::t!("settings.network.reset_bypass").into_owned(),
        )
        .leading_icon(symbol_icon(Symbol::RESTART_ALT))
        .on_activate(move |window, cx| {
            let _ = reset_view.update(cx, |this, cx| {
                this.reset_bypass(window, cx);
            });
        });

        SectionCard::new()
            .title(rust_i18n::t!("settings.network.title").into_owned())
            .subtitle(rust_i18n::t!("settings.network.subtitle").into_owned())
            .child(div().w(px(260.0)).max_w_full().child(port_field))
            .child(allow_lan_switch)
            .child(ipv6_switch)
            .child(bypass_field)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(px(10.0))
                    .child(apply_btn)
                    .child(reset_bypass_btn),
            )
    }

    fn render_tun_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let settings = self.store.read(cx).settings().clone();
        let tun = &settings.syncable.tun;
        let local_iface = settings.local.tun_interface_name.clone();
        let inputs = self
            .inputs
            .as_ref()
            .expect("settings inputs must be initialized before render");

        let current_stack = normalize_tun_stack(&tun.stack).unwrap_or("mixed");
        let stack_commands = self.commands.clone();
        let stack_tun = tun.clone();
        let stack_iface = local_iface.clone();
        let stack_selector = ButtonGroup::connected(
            "settings-tun-stack",
            rust_i18n::t!("settings.tun.stack_label").into_owned(),
            [
                ButtonGroupItem::new(
                    "system",
                    rust_i18n::t!("settings.tun.stack.system").into_owned(),
                ),
                ButtonGroupItem::new(
                    "gvisor",
                    rust_i18n::t!("settings.tun.stack.gvisor").into_owned(),
                ),
                ButtonGroupItem::new(
                    "mixed",
                    rust_i18n::t!("settings.tun.stack.mixed").into_owned(),
                ),
            ],
        )
        .selection(ButtonGroupSelection::SingleRequired, [current_stack.into()])
        .on_change(move |selected, _, _| {
            if let Some(first) = selected.first()
                && let Some(stack) = normalize_tun_stack(first.as_ref())
            {
                let mut next = stack_tun.clone();
                next.stack = stack.to_owned();
                stack_commands.update_tun_settings(next, stack_iface.clone());
            }
        });

        let auto_route_commands = self.commands.clone();
        let auto_route_tun = tun.clone();
        let auto_route_iface = local_iface.clone();
        let auto_route_switch = SwitchTile::new(
            "settings-tun-auto-route",
            rust_i18n::t!("settings.tun.auto_route").into_owned(),
            tun.auto_route,
        )
        .supporting(rust_i18n::t!("settings.tun.auto_route_supporting").into_owned())
        .on_change(move |enabled, _, _| {
            let mut next = auto_route_tun.clone();
            next.auto_route = enabled;
            auto_route_commands.update_tun_settings(next, auto_route_iface.clone());
        });

        let strict_route_commands = self.commands.clone();
        let strict_route_tun = tun.clone();
        let strict_route_iface = local_iface;
        let strict_route_switch = SwitchTile::new(
            "settings-tun-strict-route",
            rust_i18n::t!("settings.tun.strict_route").into_owned(),
            tun.strict_route,
        )
        .supporting(rust_i18n::t!("settings.tun.strict_route_supporting").into_owned())
        .on_change(move |enabled, _, _| {
            let mut next = strict_route_tun.clone();
            next.strict_route = enabled;
            strict_route_commands.update_tun_settings(next, strict_route_iface.clone());
        });

        let mut mtu_field = TextField::outlined(
            "settings-tun-mtu",
            rust_i18n::t!("settings.tun.mtu_label").into_owned(),
            &inputs.tun_mtu,
        );
        mtu_field = if let Some(err) = self.tun_mtu_error.as_ref() {
            mtu_field.error(err.clone())
        } else {
            mtu_field.supporting_text(rust_i18n::t!("settings.tun.mtu_supporting").into_owned())
        };

        let dns_hijack_field = TextField::outlined(
            "settings-tun-dns-hijack",
            rust_i18n::t!("settings.tun.dns_hijack_label").into_owned(),
            &inputs.tun_dns_hijack,
        )
        .supporting_text(rust_i18n::t!("settings.tun.dns_hijack_supporting").into_owned());

        let exclude_routes_field = TextField::outlined(
            "settings-tun-exclude-routes",
            rust_i18n::t!("settings.tun.exclude_routes_label").into_owned(),
            &inputs.tun_exclude_routes,
        )
        .supporting_text(rust_i18n::t!("settings.tun.exclude_routes_supporting").into_owned());

        let interface_field = TextField::outlined(
            "settings-tun-interface",
            rust_i18n::t!("settings.tun.interface_label").into_owned(),
            &inputs.tun_interface,
        )
        .supporting_text(rust_i18n::t!("settings.tun.interface_supporting").into_owned());

        let apply_view = cx.weak_entity();
        let apply_btn = Button::filled(
            "settings-tun-apply",
            rust_i18n::t!("settings.tun.apply").into_owned(),
        )
        .leading_icon(symbol_icon(Symbol::CHECK))
        .on_activate(move |window, cx| {
            let _ = apply_view.update(cx, |this, cx| {
                this.apply_tun_form(window, cx);
            });
        });

        let defaults_view = cx.weak_entity();
        let defaults_btn = Button::outlined(
            "settings-tun-safe-defaults",
            rust_i18n::t!("settings.tun.safe_defaults").into_owned(),
        )
        .leading_icon(symbol_icon(Symbol::RESTART_ALT))
        .on_activate(move |window, cx| {
            let _ = defaults_view.update(cx, |this, cx| {
                this.apply_tun_safe_defaults(window, cx);
            });
        });

        SectionCard::new()
            .title(rust_i18n::t!("settings.tun.title").into_owned())
            .subtitle(rust_i18n::t!("settings.tun.subtitle").into_owned())
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
                            .child(rust_i18n::t!("settings.tun.stack_label").into_owned()),
                    )
                    .child(stack_selector),
            )
            .child(auto_route_switch)
            .child(strict_route_switch)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(12.0))
                    .child(div().w(px(220.0)).max_w_full().child(mtu_field))
                    .child(div().flex_1().min_w(px(220.0)).child(interface_field)),
            )
            .child(dns_hijack_field)
            .child(exclude_routes_field)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(px(10.0))
                    .child(apply_btn)
                    .child(defaults_btn),
            )
    }

    fn render_service_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.store.read(cx);
        let local = store.settings().local.clone();
        let service_status = store.runtime().service_status.clone();

        let (health_state, health_label) =
            service_status_badge(&service_status, local.service_mode_enabled);

        let unavailable_reason = match &service_status {
            ServiceStatus::Unavailable { reason: Some(msg) } if !msg.trim().is_empty() => {
                Some(msg.clone())
            }
            _ => None,
        };

        let mode_commands = self.commands.clone();
        let service_mode_switch = SwitchTile::new(
            "settings-service-mode",
            rust_i18n::t!("settings.machine.service_mode").into_owned(),
            local.service_mode_enabled,
        )
        .supporting(rust_i18n::t!("settings.service.enable_supporting").into_owned())
        .on_change(move |enabled, _, _| {
            mode_commands.set_service_mode(enabled);
        });

        let check_commands = self.commands.clone();
        let check_btn = Button::outlined(
            "settings-service-check",
            rust_i18n::t!("settings.service.check_status").into_owned(),
        )
        .leading_icon(symbol_icon(Symbol::REFRESH))
        .on_activate(move |_, _| {
            check_commands.service_action(ServiceControlAction::CheckStatus);
        });

        let install_commands = self.commands.clone();
        let install_btn = Button::tonal(
            "settings-service-install",
            rust_i18n::t!("settings.service.install").into_owned(),
        )
        .leading_icon(symbol_icon(Symbol::DOWNLOAD))
        .on_activate(move |_, _| {
            install_commands.service_action(ServiceControlAction::Install);
        });

        let uninstall_view = cx.weak_entity();
        let uninstall_btn = Button::outlined(
            "settings-service-uninstall",
            rust_i18n::t!("settings.service.uninstall").into_owned(),
        )
        .tone(SemanticTone::Danger)
        .leading_icon(symbol_icon(Symbol::DELETE))
        .on_activate(move |window, cx| {
            let _ = uninstall_view.update(cx, |this, cx| {
                this.request_service_uninstall(window, cx);
            });
        });

        let guidance_banner = Banner::new(
            "settings-service-guidance",
            rust_i18n::t!(platform_service_guidance_key()).into_owned(),
        )
        .title(rust_i18n::t!("settings.service.guidance_title").into_owned())
        .tone(SemanticTone::Info)
        .icon(symbol_icon(Symbol::INFO));

        let mut card = SectionCard::new()
            .title(rust_i18n::t!("settings.service.title").into_owned())
            .subtitle(rust_i18n::t!("settings.service.subtitle").into_owned())
            .header_trailing(HealthIndicator::new(health_state).label(health_label))
            .child(service_mode_switch);

        if let Some(reason) = unavailable_reason {
            card = card.child(
                Banner::new("settings-service-unavailable-detail", reason)
                    .title(rust_i18n::t!("settings.service.unavailable_reason_title").into_owned())
                    .tone(SemanticTone::Warning)
                    .icon(symbol_icon(Symbol::SHIELD)),
            );
        }

        card.child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(px(10.0))
                .child(check_btn)
                .child(install_btn)
                .child(uninstall_btn),
        )
        .child(guidance_banner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_service_status_variants_to_health_states() {
        let (state, _) = service_status_badge(&ServiceStatus::Unknown, false);
        assert_eq!(state, KernelHealthState::Stopped);

        let (state, _) = service_status_badge(
            &ServiceStatus::Available {
                running_pid: Some(4242),
            },
            true,
        );
        assert_eq!(state, KernelHealthState::Running);

        let (state, _) =
            service_status_badge(&ServiceStatus::Available { running_pid: None }, false);
        assert_eq!(state, KernelHealthState::Running);

        let (state, _) = service_status_badge(
            &ServiceStatus::Unavailable {
                reason: Some("pipe not found".into()),
            },
            true,
        );
        assert_eq!(state, KernelHealthState::Fault);

        let (state, _) = service_status_badge(
            &ServiceStatus::Unavailable {
                reason: Some("pipe not found".into()),
            },
            false,
        );
        assert_eq!(state, KernelHealthState::Stopped);
    }

    #[test]
    fn selects_platform_guidance_key() {
        let key = platform_service_guidance_key();
        if cfg!(target_os = "linux") {
            assert_eq!(key, "settings.service.guidance_linux");
        } else {
            assert_eq!(key, "settings.service.guidance_desktop");
        }
    }
}
