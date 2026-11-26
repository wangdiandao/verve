//! Profiles feature slice: remote subscription import, local YAML creation/file import,
//! profile registry cards with quota & expiry telemetry, activation, refresh, deletion,
//! and scoped detail / import side sheets.

pub mod import_sheet;

use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Render, ScrollHandle, StatefulInteractiveElement, Styled, Subscription, Window, div,
    prelude::FluentBuilder, px,
};
use gpui_material::{
    Banner, Button, ButtonVariant, IconButton, LinearProgress, SemanticTone, TextField,
    TextFieldEvent, TextFieldState,
};
use gpui_material_symbols::Symbol;
use verve_core::DomainKind;
use verve_core::config::{ProfileEntry, ProfileSource, ProfileUsage};
use verve_ui_kit::{
    EmptyState, HealthIndicator, KernelHealthState, MetricTile, SectionCard, format_bytes,
    format_unix_date, format_unix_datetime, theme_tokens,
};

pub use import_sheet::{DEFAULT_LOCAL_YAML_TEMPLATE, LocalImportMode, NewLocalProfileSheet};

use crate::bridge::{CommandDispatcher, DomainChanged, UiStore};
use crate::shell::nav::symbol_icon;
use crate::shell::sheet_host::{ConfirmDialogSpec, ShellHandle, SideSheetSpec};

const EXPIRING_SOON_WINDOW_SECS: i64 = 7 * 86_400;
const LOCAL_SHEET_WIDTH_PX: f32 = 440.0;
const DETAIL_SHEET_WIDTH_PX: f32 = 420.0;

/// Subscription expiration classification relative to the current Unix timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpirationStatus {
    /// Subscription does not specify an expiration timestamp (`expire_unix_secs == None`).
    Never,
    /// Subscription expires later than the imminent-expiry warning window.
    Valid,
    /// Subscription expires within 7 days (`0 <= expire - now <= 7 days`).
    ExpiringSoon,
    /// Subscription expiration timestamp is strictly in the past (`expire < now`).
    Expired,
}

/// Returns `true` when `url` is a non-empty `http://` or `https://` URL with a non-empty authority.
pub fn is_valid_subscription_url(url: &str) -> bool {
    let trimmed = url.trim();
    let lower = trimmed.to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &trimmed["https://".len()..]
    } else if lower.starts_with("http://") {
        &trimmed["http://".len()..]
    } else {
        return false;
    };
    !rest.trim().is_empty() && !rest.chars().any(char::is_whitespace)
}

/// Computes the clamped traffic quota usage fraction in `0.0..=1.0`.
///
/// Returns `None` when `usage.total_bytes` is `None` or `Some(0)` (unlimited/unspecified quota).
pub fn compute_quota_fraction(usage: &ProfileUsage) -> Option<f32> {
    let total = usage.total_bytes.filter(|&bytes| bytes > 0)?;
    let used = usage.upload_bytes.saturating_add(usage.download_bytes);
    let ratio = (used as f64 / total as f64).clamp(0.0, 1.0);
    Some(ratio as f32)
}

/// Classifies an optional subscription expiration timestamp against `now_unix_secs`.
pub const fn classify_expiration(
    expire_unix_secs: Option<i64>,
    now_unix_secs: i64,
) -> ExpirationStatus {
    match expire_unix_secs {
        None => ExpirationStatus::Never,
        Some(expire) if expire < now_unix_secs => ExpirationStatus::Expired,
        Some(expire) if expire.saturating_sub(now_unix_secs) <= EXPIRING_SOON_WINDOW_SECS => {
            ExpirationStatus::ExpiringSoon
        }
        Some(_) => ExpirationStatus::Valid,
    }
}

fn quota_tone(fraction: f32) -> SemanticTone {
    if fraction >= 0.95 {
        SemanticTone::Danger
    } else if fraction >= 0.80 {
        SemanticTone::Warning
    } else {
        SemanticTone::Default
    }
}

fn current_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Vertical feature view managing remote subscriptions, local YAML profiles, activation,
/// traffic quotas, expiration metadata, and scoped inspection/import drawers.
pub struct ProfilesView {
    store: Entity<UiStore>,
    commands: CommandDispatcher,
    shell: ShellHandle,
    scroll: ScrollHandle,
    url_input: Option<Entity<TextFieldState>>,
    url_error: Option<String>,
    _url_input_sub: Option<Subscription>,
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
            url_input: None,
            url_error: None,
            _url_input_sub: None,
            _store_sub: store_sub,
        }
    }

    /// Returns the inline remote URL validation error, if any.
    pub fn url_error(&self) -> Option<&str> {
        self.url_error.as_deref()
    }

    /// Sets the remote subscription URL input programmatically.
    pub fn set_remote_url_input(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.ensure_url_input(window, cx);
        let value = url.to_owned();
        input.update(cx, |state, cx| {
            state.set_value(value, window, cx);
        });
        self.url_error = None;
        cx.notify();
    }

    /// Validates and submits the current remote subscription URL input.
    ///
    /// Returns `true` when validation succeeded and `import_remote_profile` was dispatched.
    pub fn submit_remote_url(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let input = self.ensure_url_input(window, cx);
        let raw = input.read(cx).value().to_string();
        let trimmed = raw.trim();
        if !is_valid_subscription_url(trimmed) {
            self.url_error = Some(rust_i18n::t!("profiles.import_bar.invalid_url").into_owned());
            cx.notify();
            return false;
        }

        self.url_error = None;
        self.commands
            .import_remote_profile(trimmed.to_owned(), None);
        self.commands
            .notify_info(rust_i18n::t!("profiles.importing_toast").into_owned());
        input.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });
        cx.notify();
        true
    }

    /// Opens the scoped [`NewLocalProfileSheet`] in the shell's right-hand side sheet host.
    pub fn open_new_local_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let commands = self.commands.clone();
        let shell = self.shell.clone();
        let sheet_title = rust_i18n::t!("profiles.local_sheet.title").into_owned();
        let sheet_view = cx.new(|cx| NewLocalProfileSheet::new(store, commands, shell, window, cx));
        self.shell.open_sheet(
            SideSheetSpec::new(sheet_title, sheet_view.into()).width(px(LOCAL_SHEET_WIDTH_PX)),
            window,
            cx,
        );
    }

    /// Opens the scoped detail side sheet for the profile with `profile_id`.
    ///
    /// Returns `false` if no registered profile matches `profile_id`.
    pub fn open_profile_detail(
        &mut self,
        profile_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let profiles = self.store.read(cx).profiles().clone();
        let Some(entry) = profiles
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .cloned()
        else {
            return false;
        };
        let usage = profiles.usages.get(&entry.id).cloned();
        let is_active = profiles.active_profile_id.as_deref() == Some(entry.id.as_str());
        let sheet_title = rust_i18n::t!("profiles.sheet.title", name = entry.name).into_owned();
        let detail_view = cx.new(|_| ProfileDetailSheet {
            entry,
            usage,
            active: is_active,
            scroll: ScrollHandle::new(),
        });
        self.shell.open_sheet(
            SideSheetSpec::new(sheet_title, detail_view.into()).width(px(DETAIL_SHEET_WIDTH_PX)),
            window,
            cx,
        );
        true
    }

    /// Opens a destructive confirmation dialog to delete the profile with `profile_id`.
    ///
    /// Returns `false` if no registered profile matches `profile_id`.
    pub fn open_delete_confirm(
        &mut self,
        profile_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let profiles = self.store.read(cx).profiles().clone();
        let Some(entry) = profiles
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .cloned()
        else {
            return false;
        };

        let delete_cmd = self.commands.clone();
        let delete_id = entry.id.clone();
        let title = rust_i18n::t!("profiles.delete_confirm.title", name = entry.name).into_owned();
        let description = rust_i18n::t!("profiles.delete_confirm.desc").into_owned();
        let confirm_label = rust_i18n::t!("common.delete").into_owned();

        self.shell.open_confirm(
            ConfirmDialogSpec::new(title, description, confirm_label, move |_, _| {
                delete_cmd.delete_profile(delete_id.clone());
            })
            .destructive(true),
            window,
            cx,
        );
        true
    }

    fn ensure_url_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextFieldState> {
        if let Some(existing) = self.url_input.as_ref() {
            return existing.clone();
        }

        let input = cx.new(|cx| TextFieldState::new(window, cx).placeholder("https://..."));
        let sub = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &TextFieldEvent, window, cx| match event {
                TextFieldEvent::Change => {
                    if this.url_error.take().is_some() {
                        cx.notify();
                    }
                }
                TextFieldEvent::PressEnter { .. } => {
                    this.submit_remote_url(window, cx);
                }
                _ => {}
            },
        );
        self.url_input = Some(input.clone());
        self._url_input_sub = Some(sub);
        input
    }
}

impl Render for ProfilesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let url_input = self.ensure_url_input(window, cx);
        let store = self.store.read(cx);
        let profiles = store.profiles().clone();
        let now_unix = current_unix_secs();

        let weak_new = cx.weak_entity();
        let page_header = div()
            .w_full()
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
                    .child(rust_i18n::t!("profiles.title").into_owned()),
            )
            .child(
                Button::new(
                    "profiles-new-local-btn",
                    rust_i18n::t!("profiles.new_local_btn").into_owned(),
                )
                .variant(ButtonVariant::FilledTonal)
                .leading_icon(symbol_icon(Symbol::ADD))
                .on_activate(move |window, cx| {
                    let _ = weak_new.update(cx, |this, cx| {
                        this.open_new_local_sheet(window, cx);
                    });
                }),
            );

        let mut url_field = TextField::outlined(
            "profiles-url-input",
            rust_i18n::t!("profiles.import_bar.url_label").into_owned(),
            &url_input,
        )
        .show_label(false)
        .clearable(true)
        .min_height(px(40.0));

        if let Some(err) = self.url_error.as_ref() {
            url_field = url_field.error(err.clone());
        }

        let weak_import = cx.weak_entity();
        let import_bar = SectionCard::new()
            .title(rust_i18n::t!("profiles.import_bar.title").into_owned())
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(12.0))
                    .child(div().flex_1().min_w_0().child(url_field))
                    .child(
                        div().flex_none().child(
                            Button::new(
                                "profiles-url-import-btn",
                                rust_i18n::t!("common.import").into_owned(),
                            )
                            .variant(ButtonVariant::Filled)
                            .leading_icon(symbol_icon(Symbol::DOWNLOAD))
                            .on_activate(move |window, cx| {
                                let _ = weak_import.update(cx, |this, cx| {
                                    this.submit_remote_url(window, cx);
                                });
                            }),
                        ),
                    ),
            );

        let mut root = div()
            .id("profiles-page")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p(px(20.0))
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(page_header)
            .child(import_bar);

        if let Some(diag) = profiles.last_compile_diagnostic.as_ref() {
            root = root.child(
                Banner::new("profiles-compile-diagnostic", diag.to_string())
                    .title(rust_i18n::t!("profiles.compile_failed").into_owned())
                    .tone(SemanticTone::Danger),
            );
        }

        if profiles.profiles.is_empty() {
            let weak_empty = cx.weak_entity();
            return root.child(
                EmptyState::new(
                    rust_i18n::t!("profiles.empty.title").into_owned(),
                    rust_i18n::t!("profiles.empty.desc").into_owned(),
                )
                .action(
                    Button::new(
                        "profiles-empty-new-local",
                        rust_i18n::t!("profiles.new_local_btn").into_owned(),
                    )
                    .variant(ButtonVariant::FilledTonal)
                    .leading_icon(symbol_icon(Symbol::ADD))
                    .on_activate(move |window, cx| {
                        let _ = weak_empty.update(cx, |this, cx| {
                            this.open_new_local_sheet(window, cx);
                        });
                    }),
                ),
            );
        }

        for (idx, entry) in profiles.profiles.iter().enumerate() {
            let is_active = profiles.active_profile_id.as_deref() == Some(entry.id.as_str());
            let is_remote = matches!(entry.source, ProfileSource::Remote { .. });
            let usage = profiles.usages.get(&entry.id).cloned();

            let source_desc = match &entry.source {
                ProfileSource::Local => {
                    rust_i18n::t!("profiles.source_local", file = entry.file_name).into_owned()
                }
                ProfileSource::Remote { url, .. } => {
                    rust_i18n::t!("profiles.source_remote", url = url).into_owned()
                }
            };

            let active_label = rust_i18n::t!("profiles.status.active").into_owned();
            let inactive_label = rust_i18n::t!("profiles.status.inactive").into_owned();

            let header_activate_cmd = self.commands.clone();
            let header_activate_id = entry.id.clone();

            let header = div()
                .id(("profile-card-header", idx))
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(12.0))
                .when(!is_active, |el| {
                    el.cursor_pointer().on_click(move |_, _, _| {
                        header_activate_cmd.activate_profile(header_activate_id.clone());
                    })
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_base()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(tokens.on_surface)
                                .child(entry.name.clone()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(tokens.on_surface_variant)
                                .child(source_desc),
                        ),
                )
                .child(div().flex_none().child(if is_active {
                    HealthIndicator::new(KernelHealthState::Running).label(active_label.clone())
                } else {
                    HealthIndicator::new(KernelHealthState::Stopped).label(inactive_label)
                }));

            let activate_cmd = self.commands.clone();
            let activate_id = entry.id.clone();

            let mut left_actions = div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(px(8.0))
                .child({
                    let mut btn = Button::new(
                        ("profile-activate", idx),
                        if is_active {
                            active_label
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
                    });
                    if is_active {
                        btn = btn.leading_icon(symbol_icon(Symbol::CHECK));
                    }
                    btn
                });

            if is_remote {
                let refresh_cmd = self.commands.clone();
                let refresh_id = entry.id.clone();
                let refresh_name = entry.name.clone();
                left_actions = left_actions.child(
                    Button::new(
                        ("profile-refresh", idx),
                        rust_i18n::t!("common.refresh").into_owned(),
                    )
                    .variant(ButtonVariant::Outlined)
                    .leading_icon(symbol_icon(Symbol::REFRESH))
                    .on_activate(move |_, _| {
                        refresh_cmd.refresh_profile(refresh_id.clone());
                        refresh_cmd.notify_info(
                            rust_i18n::t!("profiles.refreshing_toast", name = refresh_name)
                                .into_owned(),
                        );
                    }),
                );
            }

            let weak_detail = cx.weak_entity();
            let detail_id = entry.id.clone();
            left_actions = left_actions.child(
                Button::new(
                    ("profile-inspect", idx),
                    rust_i18n::t!("profiles.details_btn").into_owned(),
                )
                .variant(ButtonVariant::Outlined)
                .on_activate(move |window, cx| {
                    let _ = weak_detail.update(cx, |this, cx| {
                        this.open_profile_detail(&detail_id, window, cx);
                    });
                }),
            );

            let weak_delete = cx.weak_entity();
            let delete_id = entry.id.clone();
            let delete_btn = IconButton::new(
                ("profile-delete", idx),
                rust_i18n::t!("profiles.delete_btn").into_owned(),
                symbol_icon(Symbol::DELETE),
            )
            .tone(SemanticTone::Danger)
            .on_activate(move |window, cx| {
                let _ = weak_delete.update(cx, |this, cx| {
                    this.open_delete_confirm(&delete_id, window, cx);
                });
            });

            let actions = div()
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(8.0))
                .child(left_actions)
                .child(div().flex_none().child(delete_btn));

            let mut card = div()
                .id(("profile-card", idx))
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(12.0))
                .p(px(16.0))
                .rounded(px(16.0))
                .bg(if is_active {
                    tokens.surface_container
                } else {
                    tokens.surface_container_low
                })
                .border_1()
                .border_color(if is_active {
                    tokens.primary
                } else {
                    tokens.outline_variant
                })
                .child(header);

            if let Some(u) = usage.as_ref() {
                card = card.child(render_usage_section(idx, u, now_unix, &tokens));
            }

            card = card.child(actions);
            root = root.child(card);
        }

        root
    }
}

fn render_usage_section(
    idx: usize,
    usage: &ProfileUsage,
    now_unix: i64,
    tokens: &verve_ui_kit::ThemeTokens,
) -> impl IntoElement {
    let used_bytes = usage.upload_bytes.saturating_add(usage.download_bytes);
    let mut section = div().w_full().flex().flex_col().gap(px(6.0));

    if let (Some(total), Some(fraction)) = (usage.total_bytes, compute_quota_fraction(usage)) {
        let percent = format!("{:.0}%", fraction * 100.0);
        section = section
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(tokens.on_surface)
                            .child(
                                rust_i18n::t!(
                                    "profiles.quota_used_total",
                                    used = format_bytes(used_bytes),
                                    total = format_bytes(total)
                                )
                                .into_owned(),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(tokens.on_surface_variant)
                            .child(percent),
                    ),
            )
            .child(
                LinearProgress::new(
                    ("profile-quota-progress", idx),
                    rust_i18n::t!("profiles.quota_label").into_owned(),
                )
                .value(fraction)
                .tone(quota_tone(fraction)),
            );
    } else {
        section = section.child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(tokens.on_surface_variant)
                .child(
                    rust_i18n::t!("profiles.quota_unlimited", used = format_bytes(used_bytes))
                        .into_owned(),
                ),
        );
    }

    let expiry_status = classify_expiration(usage.expire_unix_secs, now_unix);
    let (expiry_text, expiry_color, expiry_weight) = match (expiry_status, usage.expire_unix_secs) {
        (ExpirationStatus::Never, _) => (
            rust_i18n::t!(
                "profiles.expires_label",
                date = rust_i18n::t!("common.never").into_owned()
            )
            .into_owned(),
            tokens.on_surface_variant,
            FontWeight::NORMAL,
        ),
        (ExpirationStatus::Expired, Some(expire)) => (
            rust_i18n::t!("profiles.expired_label", date = format_unix_date(expire)).into_owned(),
            tokens.error,
            FontWeight::MEDIUM,
        ),
        (ExpirationStatus::ExpiringSoon, Some(expire)) => (
            rust_i18n::t!("profiles.expires_label", date = format_unix_date(expire)).into_owned(),
            tokens.tertiary,
            FontWeight::MEDIUM,
        ),
        (_, Some(expire)) => (
            rust_i18n::t!("profiles.expires_label", date = format_unix_date(expire)).into_owned(),
            tokens.on_surface_variant,
            FontWeight::NORMAL,
        ),
        (_, None) => (
            rust_i18n::t!(
                "profiles.expires_label",
                date = rust_i18n::t!("common.never").into_owned()
            )
            .into_owned(),
            tokens.on_surface_variant,
            FontWeight::NORMAL,
        ),
    };

    let updated_text = rust_i18n::t!(
        "profiles.updated_at_label",
        time = format_unix_datetime(usage.updated_at_unix_secs)
    )
    .into_owned();

    section.child(
        div()
            .w_full()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(px(16.0))
            .child(
                div()
                    .text_xs()
                    .font_weight(expiry_weight)
                    .text_color(expiry_color)
                    .child(expiry_text),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(tokens.on_surface_variant)
                    .child(updated_text),
            ),
    )
}

struct ProfileDetailSheet {
    entry: ProfileEntry,
    usage: Option<ProfileUsage>,
    active: bool,
    scroll: ScrollHandle,
}

impl Render for ProfileDetailSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let now_unix = current_unix_secs();

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

        let mut content = div()
            .id("profile-detail-sheet")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
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
            .child(
                div()
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
                            .child(rust_i18n::t!("profiles.sheet.source").into_owned()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(tokens.on_surface)
                            .child(source_label),
                    ),
            )
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
            ));

        if let Some(u) = self.usage.as_ref() {
            let used_bytes = u.upload_bytes.saturating_add(u.download_bytes);
            let quota_value = match u.total_bytes.filter(|&b| b > 0) {
                Some(total) => format!("{} / {}", format_bytes(used_bytes), format_bytes(total)),
                None => format!(
                    "{} ({})",
                    format_bytes(used_bytes),
                    rust_i18n::t!("profiles.quota_unlimited_value").into_owned()
                ),
            };

            let expiry_status = classify_expiration(u.expire_unix_secs, now_unix);
            let expiry_value = match (expiry_status, u.expire_unix_secs) {
                (ExpirationStatus::Never, _) | (_, None) => {
                    rust_i18n::t!("common.never").into_owned()
                }
                (ExpirationStatus::Expired, Some(expire)) => {
                    rust_i18n::t!("profiles.expired_label", date = format_unix_date(expire))
                        .into_owned()
                }
                (_, Some(expire)) => format_unix_date(expire),
            };

            let usage_tile = MetricTile::new(
                rust_i18n::t!("profiles.sheet.traffic_quota").into_owned(),
                quota_value,
            )
            .sublabel(
                rust_i18n::t!(
                    "profiles.sheet.recorded_usage",
                    down = format_bytes(u.download_bytes),
                    up = format_bytes(u.upload_bytes)
                )
                .into_owned(),
            );
            if let Some(fraction) = compute_quota_fraction(u) {
                content = content.child(usage_tile).child(
                    LinearProgress::new(
                        "profile-detail-quota-progress",
                        rust_i18n::t!("profiles.quota_label").into_owned(),
                    )
                    .value(fraction)
                    .tone(quota_tone(fraction)),
                );
            } else {
                content = content.child(usage_tile);
            }

            let mut expires_tile = MetricTile::new(
                rust_i18n::t!("profiles.sheet.expires").into_owned(),
                expiry_value,
            );
            if expiry_status == ExpirationStatus::Expired {
                expires_tile = expires_tile.value_role(gpui_material::theme::ColorRole::Error);
            }

            content = content.child(expires_tile).child(MetricTile::new(
                rust_i18n::t!("profiles.sheet.last_updated").into_owned(),
                format_unix_datetime(u.updated_at_unix_secs),
            ));
        } else {
            content = content.child(
                div()
                    .text_xs()
                    .text_color(tokens.on_surface_variant)
                    .child(rust_i18n::t!("profiles.sheet.no_usage").into_owned()),
            );
        }

        content
    }
}

#[cfg(test)]
mod tests {
    use verve_core::validate_profile_yaml;

    use super::*;

    fn sample_usage(upload: u64, download: u64, total: Option<u64>) -> ProfileUsage {
        ProfileUsage {
            profile_id: "sub-1".into(),
            upload_bytes: upload,
            download_bytes: download,
            total_bytes: total,
            expire_unix_secs: None,
            updated_at_unix_secs: 1_750_000_000,
        }
    }

    #[test]
    fn quota_fraction_handles_zero_partial_full_and_overflow() {
        assert_eq!(compute_quota_fraction(&sample_usage(100, 200, None)), None);
        assert_eq!(
            compute_quota_fraction(&sample_usage(100, 200, Some(0))),
            None
        );
        assert_eq!(
            compute_quota_fraction(&sample_usage(0, 0, Some(1_000))),
            Some(0.0)
        );

        let half = compute_quota_fraction(&sample_usage(250, 250, Some(1_000))).unwrap();
        assert!((half - 0.5).abs() < 1e-6);

        let full = compute_quota_fraction(&sample_usage(600, 400, Some(1_000))).unwrap();
        assert!((full - 1.0).abs() < 1e-6);

        // Over-quota usage and saturating u64 addition must clamp cleanly to 1.0.
        assert_eq!(
            compute_quota_fraction(&sample_usage(900, 900, Some(1_000))),
            Some(1.0)
        );
        assert_eq!(
            compute_quota_fraction(&sample_usage(u64::MAX, 100, Some(1_000))),
            Some(1.0)
        );
    }

    #[test]
    fn expiration_status_classifies_never_valid_soon_and_expired() {
        let now = 1_800_000_000_i64;
        assert_eq!(classify_expiration(None, now), ExpirationStatus::Never);
        assert_eq!(
            classify_expiration(Some(now - 1), now),
            ExpirationStatus::Expired
        );
        assert_eq!(
            classify_expiration(Some(now), now),
            ExpirationStatus::ExpiringSoon
        );
        assert_eq!(
            classify_expiration(Some(now + EXPIRING_SOON_WINDOW_SECS), now),
            ExpirationStatus::ExpiringSoon
        );
        assert_eq!(
            classify_expiration(Some(now + EXPIRING_SOON_WINDOW_SECS + 1), now),
            ExpirationStatus::Valid
        );
    }

    #[test]
    fn subscription_url_validation_accepts_http_https_only() {
        assert!(is_valid_subscription_url(
            "https://sub.example.com/clash.yaml"
        ));
        assert!(is_valid_subscription_url("  http://127.0.0.1:8080/sub  "));
        assert!(is_valid_subscription_url("HTTPS://SUB.EXAMPLE.COM/get?t=1"));

        assert!(!is_valid_subscription_url(""));
        assert!(!is_valid_subscription_url("   "));
        assert!(!is_valid_subscription_url("https://"));
        assert!(!is_valid_subscription_url("http://   "));
        assert!(!is_valid_subscription_url("ftp://example.com/sub.yaml"));
        assert!(!is_valid_subscription_url("not-a-url"));
        assert!(!is_valid_subscription_url("https://example.com/has space"));
    }

    #[test]
    fn default_local_yaml_template_is_valid() {
        assert!(
            validate_profile_yaml(DEFAULT_LOCAL_YAML_TEMPLATE).is_ok(),
            "DEFAULT_LOCAL_YAML_TEMPLATE must pass verve_core::validate_profile_yaml"
        );
    }
}
