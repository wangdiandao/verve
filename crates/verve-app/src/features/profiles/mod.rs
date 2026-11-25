//! Profiles feature slice: profile registry cards, activation, and scoped detail side sheet.

use gpui::{
    AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Render, ScrollHandle, StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use gpui_material::{Banner, Button, ButtonVariant, SemanticTone};
use verve_core::DomainKind;
use verve_core::config::{ProfileEntry, ProfileSource, ProfileUsage};
use verve_ui_kit::{
    EmptyState, HealthIndicator, KernelHealthState, MetricTile, SectionCard, format_bytes,
    theme_tokens,
};

use crate::bridge::{CommandDispatcher, DomainChanged, UiStore};
use crate::shell::sheet_host::{ShellHandle, SideSheetSpec};

/// Vertical feature view listing registered profiles and opening scoped inspection sheets.
pub struct ProfilesView {
    store: Entity<UiStore>,
    commands: CommandDispatcher,
    shell: ShellHandle,
    scroll: ScrollHandle,
    _store_sub: Subscription,
}

impl ProfilesView {
    /// Creates a new [`ProfilesView`] bound to `store`, `commands`, and `shell`.
    pub fn new(
        store: Entity<UiStore>,
        commands: CommandDispatcher,
        shell: ShellHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        let store_sub = cx.subscribe(&store, |_, _, ev: &DomainChanged, cx| {
            if matches!(ev.0, DomainKind::Profiles | DomainKind::Runtime) {
                cx.notify();
            }
        });
        Self {
            store,
            commands,
            shell,
            scroll: ScrollHandle::new(),
            _store_sub: store_sub,
        }
    }
}

impl Render for ProfilesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let store = self.store.read(cx);
        let profiles = store.profiles().clone();

        let mut root = div()
            .id("profiles-page")
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
                    .child("Profiles"),
            );

        if let Some(diag) = profiles.last_compile_diagnostic.as_ref() {
            root = root.child(
                Banner::new(
                    "profiles-compile-diagnostic",
                    format!("[{}] {}", diag.stage, diag.message),
                )
                .title("Last profile compile failed")
                .tone(SemanticTone::Danger),
            );
        }

        if profiles.profiles.is_empty() {
            return root.child(EmptyState::new(
                "No profiles configured",
                "Import a local or remote YAML profile through verve-core to compile and activate it.",
            ));
        }

        for (idx, entry) in profiles.profiles.iter().enumerate() {
            let is_active = profiles.active_profile_id.as_deref() == Some(entry.id.as_str());
            let usage = profiles.usages.get(&entry.id).cloned();

            let source_desc = match &entry.source {
                ProfileSource::Local => format!("Local · {}", entry.file_name),
                ProfileSource::Remote { url, .. } => format!("Remote · {url}"),
            };

            let activate_cmd = self.commands.clone();
            let activate_id = entry.id.clone();

            let inspect_shell = self.shell.clone();
            let inspect_entry = entry.clone();
            let inspect_usage = usage.clone();

            let actions = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    Button::new(("profile-inspect", idx), "Details")
                        .variant(ButtonVariant::Outlined)
                        .on_activate(move |window, cx| {
                            let detail_view = cx.new(|_| ProfileDetailSheet {
                                entry: inspect_entry.clone(),
                                usage: inspect_usage.clone(),
                                active: is_active,
                            });
                            inspect_shell.open_sheet(
                                SideSheetSpec::new(
                                    format!("Profile: {}", inspect_entry.name),
                                    detail_view.into(),
                                ),
                                window,
                                cx,
                            );
                        }),
                )
                .child(
                    Button::new(
                        ("profile-activate", idx),
                        if is_active { "Active" } else { "Activate" },
                    )
                    .variant(if is_active {
                        ButtonVariant::FilledTonal
                    } else {
                        ButtonVariant::Filled
                    })
                    .disabled(is_active)
                    .on_activate(move |_, _| {
                        activate_cmd.activate_profile(activate_id.clone());
                    }),
                );

            let mut card = SectionCard::new()
                .title(entry.name.clone())
                .subtitle(source_desc)
                .header_trailing(if is_active {
                    HealthIndicator::new(KernelHealthState::Running).label("Active")
                } else {
                    HealthIndicator::new(KernelHealthState::Stopped).label("Inactive")
                });

            if let Some(u) = usage {
                card = card.child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap(px(12.0))
                        .child(MetricTile::new("Download", format_bytes(u.download_bytes)))
                        .child(MetricTile::new("Upload", format_bytes(u.upload_bytes))),
                );
            }

            card = card.child(actions);
            root = root.child(card);
        }

        root
    }
}

struct ProfileDetailSheet {
    entry: ProfileEntry,
    usage: Option<ProfileUsage>,
    active: bool,
}

impl Render for ProfileDetailSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let source_label = match &self.entry.source {
            ProfileSource::Local => "Local YAML".to_owned(),
            ProfileSource::Remote {
                url,
                update_interval_mins,
            } => format!(
                "Remote ({url}, interval: {}m)",
                update_interval_mins.unwrap_or(0)
            ),
        };

        div()
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(MetricTile::new("Profile ID", self.entry.id.clone()))
            .child(MetricTile::new(
                "Status",
                if self.active { "Active" } else { "Inactive" },
            ))
            .child(MetricTile::new("Source", source_label))
            .child(MetricTile::new("YAML File", self.entry.file_name.clone()))
            .child(MetricTile::new(
                "Merge Override",
                self.entry
                    .merge_file_name
                    .clone()
                    .unwrap_or_else(|| "None".into()),
            ))
            .child(MetricTile::new(
                "Script Override",
                self.entry
                    .script_file_name
                    .clone()
                    .unwrap_or_else(|| "None".into()),
            ))
            .child(div().text_xs().text_color(tokens.on_surface_variant).child(
                self.usage.as_ref().map_or_else(
                    || "No subscription usage metadata stored in state.db.".to_owned(),
                    |u| {
                        format!(
                            "Recorded traffic: {} down / {} up",
                            format_bytes(u.download_bytes),
                            format_bytes(u.upload_bytes)
                        )
                    },
                ),
            ))
    }
}
