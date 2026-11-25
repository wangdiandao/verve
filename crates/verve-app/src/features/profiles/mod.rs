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
            if matches!(
                ev.0,
                DomainKind::Profiles | DomainKind::Runtime | DomainKind::Settings
            ) {
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
                    .child(rust_i18n::t!("profiles.title").into_owned()),
            );

        if let Some(diag) = profiles.last_compile_diagnostic.as_ref() {
            root = root.child(
                Banner::new(
                    "profiles-compile-diagnostic",
                    format!("[{}] {}", diag.stage, diag.message),
                )
                .title(rust_i18n::t!("profiles.compile_failed").into_owned())
                .tone(SemanticTone::Danger),
            );
        }

        if profiles.profiles.is_empty() {
            return root.child(EmptyState::new(
                rust_i18n::t!("profiles.empty.title").into_owned(),
                rust_i18n::t!("profiles.empty.desc").into_owned(),
            ));
        }

        for (idx, entry) in profiles.profiles.iter().enumerate() {
            let is_active = profiles.active_profile_id.as_deref() == Some(entry.id.as_str());
            let usage = profiles.usages.get(&entry.id).cloned();

            let source_desc = match &entry.source {
                ProfileSource::Local => {
                    rust_i18n::t!("profiles.source_local", file = entry.file_name).into_owned()
                }
                ProfileSource::Remote { url, .. } => {
                    rust_i18n::t!("profiles.source_remote", url = url).into_owned()
                }
            };

            let activate_cmd = self.commands.clone();
            let activate_id = entry.id.clone();

            let inspect_shell = self.shell.clone();
            let inspect_entry = entry.clone();
            let inspect_usage = usage.clone();

            let active_label = rust_i18n::t!("profiles.status.active").into_owned();
            let inactive_label = rust_i18n::t!("profiles.status.inactive").into_owned();

            let actions = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    Button::new(
                        ("profile-inspect", idx),
                        rust_i18n::t!("profiles.details_btn").into_owned(),
                    )
                    .variant(ButtonVariant::Outlined)
                    .on_activate(move |window, cx| {
                        let sheet_title =
                            rust_i18n::t!("profiles.sheet.title", name = inspect_entry.name)
                                .into_owned();
                        let detail_view = cx.new(|_| ProfileDetailSheet {
                            entry: inspect_entry.clone(),
                            usage: inspect_usage.clone(),
                            active: is_active,
                        });
                        inspect_shell.open_sheet(
                            SideSheetSpec::new(sheet_title, detail_view.into()),
                            window,
                            cx,
                        );
                    }),
                )
                .child(
                    Button::new(
                        ("profile-activate", idx),
                        if is_active {
                            active_label.clone()
                        } else {
                            rust_i18n::t!("profiles.activate_btn").into_owned()
                        },
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
                    HealthIndicator::new(KernelHealthState::Running).label(active_label)
                } else {
                    HealthIndicator::new(KernelHealthState::Stopped).label(inactive_label)
                });

            if let Some(u) = usage {
                card = card.child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap(px(12.0))
                        .child(MetricTile::new(
                            rust_i18n::t!("common.download").into_owned(),
                            format_bytes(u.download_bytes),
                        ))
                        .child(MetricTile::new(
                            rust_i18n::t!("common.upload").into_owned(),
                            format_bytes(u.upload_bytes),
                        )),
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
            ProfileSource::Local => rust_i18n::t!("profiles.sheet.source_local").into_owned(),
            ProfileSource::Remote {
                url,
                update_interval_mins,
            } => rust_i18n::t!(
                "profiles.sheet.source_remote",
                url = url,
                mins = update_interval_mins.unwrap_or(0)
            )
            .into_owned(),
        };

        let none_label = rust_i18n::t!("common.none").into_owned();
        let status_label = if self.active {
            rust_i18n::t!("profiles.status.active").into_owned()
        } else {
            rust_i18n::t!("profiles.status.inactive").into_owned()
        };

        div()
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(MetricTile::new(
                rust_i18n::t!("profiles.sheet.profile_id").into_owned(),
                self.entry.id.clone(),
            ))
            .child(MetricTile::new(
                rust_i18n::t!("profiles.sheet.status").into_owned(),
                status_label,
            ))
            .child(MetricTile::new(
                rust_i18n::t!("profiles.sheet.source").into_owned(),
                source_label,
            ))
            .child(MetricTile::new(
                rust_i18n::t!("profiles.sheet.yaml_file").into_owned(),
                self.entry.file_name.clone(),
            ))
            .child(MetricTile::new(
                rust_i18n::t!("profiles.sheet.merge_override").into_owned(),
                self.entry
                    .merge_file_name
                    .clone()
                    .unwrap_or_else(|| none_label.clone()),
            ))
            .child(MetricTile::new(
                rust_i18n::t!("profiles.sheet.script_override").into_owned(),
                self.entry.script_file_name.clone().unwrap_or(none_label),
            ))
            .child(div().text_xs().text_color(tokens.on_surface_variant).child(
                self.usage.as_ref().map_or_else(
                    || rust_i18n::t!("profiles.sheet.no_usage").into_owned(),
                    |u| {
                        rust_i18n::t!(
                            "profiles.sheet.recorded_usage",
                            down = format_bytes(u.download_bytes),
                            up = format_bytes(u.upload_bytes)
                        )
                        .into_owned()
                    },
                ),
            ))
    }
}
