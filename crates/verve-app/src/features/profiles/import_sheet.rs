//! Scoped right-hand side sheet for creating a new local YAML profile or importing one from disk.

use std::path::Path;

use gpui::{
    AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, PathPromptOptions,
    Render, ScrollHandle, StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use gpui_material::{
    Banner, Button, ButtonGroup, ButtonGroupItem, ButtonGroupSelection, ButtonVariant,
    SemanticTone, TextAreaState, TextField, TextFieldEvent, TextFieldState, TextFieldVariant,
};
use gpui_material_symbols::Symbol;
use verve_core::{generate_profile_id, validate_profile_yaml};

use crate::bridge::{CommandDispatcher, UiStore};
use crate::shell::nav::symbol_icon;
use crate::shell::sheet_host::ShellHandle;

/// Minimal valid Mihomo YAML starter template pre-filled when creating a new local profile.
pub const DEFAULT_LOCAL_YAML_TEMPLATE: &str = "\
mixed-port: 7897
allow-lan: false
mode: rule
log-level: info
proxies: []
proxy-groups:
  - name: Proxy
    type: select
    proxies:
      - DIRECT
rules:
  - MATCH,DIRECT
";

/// Active workflow tab inside [`NewLocalProfileSheet`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocalImportMode {
    /// Author a new YAML profile directly in the sheet.
    #[default]
    CreateNew,
    /// Import an existing `.yaml` / `.yml` file from local disk.
    ImportFile,
}

/// Scoped side-sheet view handling both "Create New YAML" and "Import Local File" workflows.
pub struct NewLocalProfileSheet {
    store: Entity<UiStore>,
    commands: CommandDispatcher,
    shell: ShellHandle,
    mode: LocalImportMode,
    name_input: Entity<TextFieldState>,
    yaml_input: Entity<TextAreaState>,
    file_path_input: Entity<TextFieldState>,
    error_message: Option<String>,
    scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

impl NewLocalProfileSheet {
    /// Creates a fresh [`NewLocalProfileSheet`] with pre-filled starter YAML and local editor states.
    pub fn new(
        store: Entity<UiStore>,
        commands: CommandDispatcher,
        shell: ShellHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name_placeholder = rust_i18n::t!("profiles.local_sheet.name_placeholder").into_owned();
        let yaml_placeholder = rust_i18n::t!("profiles.local_sheet.yaml_placeholder").into_owned();
        let path_placeholder =
            rust_i18n::t!("profiles.local_sheet.file_path_placeholder").into_owned();

        let name_input = cx.new(|cx| TextFieldState::new(window, cx).placeholder(name_placeholder));
        let yaml_input = cx.new(|cx| {
            TextAreaState::new(window, cx)
                .placeholder(yaml_placeholder)
                .default_value(DEFAULT_LOCAL_YAML_TEMPLATE)
                .rows(10)
        });
        let file_path_input =
            cx.new(|cx| TextFieldState::new(window, cx).placeholder(path_placeholder));

        let name_sub = cx.subscribe(&name_input, |this, _, ev: &TextFieldEvent, cx| {
            if matches!(ev, TextFieldEvent::Change) && this.error_message.take().is_some() {
                cx.notify();
            }
        });
        let yaml_sub = cx.subscribe(&yaml_input, |this, _, ev: &TextFieldEvent, cx| {
            if matches!(ev, TextFieldEvent::Change) && this.error_message.take().is_some() {
                cx.notify();
            }
        });
        let path_sub = cx.subscribe_in(
            &file_path_input,
            window,
            |this, _, ev: &TextFieldEvent, window, cx| match ev {
                TextFieldEvent::Change => {
                    if this.error_message.take().is_some() {
                        cx.notify();
                    }
                }
                TextFieldEvent::PressEnter { .. } if this.mode == LocalImportMode::ImportFile => {
                    this.submit_import_file(window, cx);
                }
                _ => {}
            },
        );

        Self {
            store,
            commands,
            shell,
            mode: LocalImportMode::CreateNew,
            name_input,
            yaml_input,
            file_path_input,
            error_message: None,
            scroll: ScrollHandle::new(),
            _subs: vec![name_sub, yaml_sub, path_sub],
        }
    }

    /// Returns the currently selected [`LocalImportMode`].
    pub const fn mode(&self) -> LocalImportMode {
        self.mode
    }

    /// Switches between [`LocalImportMode::CreateNew`] and [`LocalImportMode::ImportFile`].
    pub fn set_mode(&mut self, mode: LocalImportMode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            self.error_message = None;
            cx.notify();
        }
    }

    /// Returns the current inline validation/IO error message, if any.
    pub fn error_message(&self) -> Option<&str> {
        self.error_message.as_deref()
    }

    /// Sets the profile name input text programmatically.
    pub fn set_name(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let value = name.to_owned();
        self.name_input.update(cx, |state, cx| {
            state.set_value(value, window, cx);
        });
        self.error_message = None;
        cx.notify();
    }

    /// Sets the YAML editor text programmatically.
    pub fn set_yaml(&mut self, yaml: &str, window: &mut Window, cx: &mut Context<Self>) {
        let value = yaml.to_owned();
        self.yaml_input.update(cx, |state, cx| {
            state.set_value(value, window, cx);
        });
        self.error_message = None;
        cx.notify();
    }

    /// Sets the local file path input text programmatically.
    pub fn set_file_path(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let value = path.to_owned();
        self.file_path_input.update(cx, |state, cx| {
            state.set_value(value, window, cx);
        });
        self.error_message = None;
        cx.notify();
    }

    /// Validates and submits the active mode (`CreateNew` or `ImportFile`).
    ///
    /// Returns `true` when validation succeeded and `import_local_profile` was dispatched.
    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match self.mode {
            LocalImportMode::CreateNew => self.submit_create_new(window, cx),
            LocalImportMode::ImportFile => self.submit_import_file(window, cx),
        }
    }

    fn browse_local_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        let weak = cx.weak_entity();
        window
            .spawn(cx, async move |cx| {
                let Ok(result) = receiver.await else {
                    return;
                };
                let _ = cx.update(|window, cx| {
                    let _ = weak.update(cx, |this, cx| match result {
                        Ok(Some(paths)) => {
                            if let Some(path) = paths.into_iter().next() {
                                let path_str = path.to_string_lossy().into_owned();
                                this.file_path_input.update(cx, |state, cx| {
                                    state.set_value(path_str, window, cx);
                                });
                                let name_empty = this.name_input.read(cx).value().trim().is_empty();
                                if name_empty
                                    && let Some(stem) = path
                                        .file_stem()
                                        .and_then(|s| s.to_str())
                                        .map(str::trim)
                                        .filter(|s| !s.is_empty())
                                {
                                    let stem = stem.to_owned();
                                    this.name_input.update(cx, |state, cx| {
                                        state.set_value(stem, window, cx);
                                    });
                                }
                                this.error_message = None;
                                cx.notify();
                            }
                        }
                        Ok(None) => {}
                        Err(err) => {
                            this.error_message = Some(
                                rust_i18n::t!(
                                    "profiles.local_sheet.error_picker",
                                    error = err.to_string()
                                )
                                .into_owned(),
                            );
                            cx.notify();
                        }
                    });
                });
            })
            .detach();
    }

    fn submit_create_new(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let name = self.name_input.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.error_message =
                Some(rust_i18n::t!("profiles.local_sheet.error_empty_name").into_owned());
            cx.notify();
            return false;
        }

        let raw_yaml = self.yaml_input.read(cx).value().to_string();
        if let Err(err) = validate_profile_yaml(&raw_yaml) {
            self.error_message = Some(
                rust_i18n::t!(
                    "profiles.local_sheet.error_invalid_yaml",
                    error = err.to_string()
                )
                .into_owned(),
            );
            cx.notify();
            return false;
        }

        let id = self.next_unique_id(&name, cx);
        self.commands
            .import_local_profile(id, name.clone(), raw_yaml, None);
        self.commands.notify_info(
            rust_i18n::t!("profiles.local_sheet.created_toast", name = name).into_owned(),
        );
        self.error_message = None;
        self.shell.close_sheet(window, cx);
        true
    }

    fn submit_import_file(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let raw_path = self.file_path_input.read(cx).value().trim().to_owned();
        if raw_path.is_empty() {
            self.error_message =
                Some(rust_i18n::t!("profiles.local_sheet.error_empty_path").into_owned());
            cx.notify();
            return false;
        }

        let path = Path::new(&raw_path);
        let content = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                self.error_message = Some(
                    rust_i18n::t!(
                        "profiles.local_sheet.error_read_file",
                        error = err.to_string()
                    )
                    .into_owned(),
                );
                cx.notify();
                return false;
            }
        };

        if let Err(err) = validate_profile_yaml(&content) {
            self.error_message = Some(
                rust_i18n::t!(
                    "profiles.local_sheet.error_invalid_yaml",
                    error = err.to_string()
                )
                .into_owned(),
            );
            cx.notify();
            return false;
        }

        let entered_name = self.name_input.read(cx).value().trim().to_owned();
        let name = if !entered_name.is_empty() {
            entered_name
        } else {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::trim)
                .filter(|stem| !stem.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| rust_i18n::t!("profiles.local_sheet.default_name").into_owned())
        };

        let id = self.next_unique_id(&name, cx);
        self.commands
            .import_local_profile(id, name.clone(), content, None);
        self.commands.notify_info(
            rust_i18n::t!("profiles.local_sheet.imported_toast", name = name).into_owned(),
        );
        self.error_message = None;
        self.shell.close_sheet(window, cx);
        true
    }

    fn next_unique_id(&self, name: &str, cx: &Context<Self>) -> String {
        let profiles = self.store.read(cx).profiles();
        let existing_ids: Vec<&str> = profiles.profiles.iter().map(|p| p.id.as_str()).collect();
        generate_profile_id(name, &existing_ids)
    }
}

impl Render for NewLocalProfileSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_mode = match self.mode {
            LocalImportMode::CreateNew => "create",
            LocalImportMode::ImportFile => "file",
        };

        let weak_mode = cx.weak_entity();
        let mode_switcher = ButtonGroup::connected(
            "local-profile-mode-group",
            rust_i18n::t!("profiles.local_sheet.mode_label").into_owned(),
            [
                ButtonGroupItem::new(
                    "create",
                    rust_i18n::t!("profiles.local_sheet.mode_create").into_owned(),
                ),
                ButtonGroupItem::new(
                    "file",
                    rust_i18n::t!("profiles.local_sheet.mode_file").into_owned(),
                ),
            ],
        )
        .selection(ButtonGroupSelection::SingleRequired, [selected_mode.into()])
        .on_change(move |selected, _, cx| {
            if let Some(first) = selected.first() {
                let next = match first.as_ref() {
                    "file" => LocalImportMode::ImportFile,
                    _ => LocalImportMode::CreateNew,
                };
                let _ = weak_mode.update(cx, |this, cx| {
                    this.set_mode(next, cx);
                });
            }
        });

        let mut body = div()
            .id("new-local-profile-sheet")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(mode_switcher);

        if let Some(err) = self.error_message.as_ref() {
            body = body
                .child(Banner::new("local-profile-error", err.clone()).tone(SemanticTone::Danger));
        }

        let mut name_field = TextField::outlined(
            "local-profile-name",
            rust_i18n::t!("profiles.local_sheet.name_label").into_owned(),
            &self.name_input,
        )
        .clearable(true);

        if self.mode == LocalImportMode::CreateNew {
            name_field = name_field.required(true);
        }

        body = body.child(name_field);

        match self.mode {
            LocalImportMode::CreateNew => {
                let yaml_field = TextField::multiline(
                    "local-profile-yaml",
                    rust_i18n::t!("profiles.local_sheet.yaml_label").into_owned(),
                    &self.yaml_input,
                )
                .variant(TextFieldVariant::Outlined)
                .required(true);
                body = body.child(yaml_field);
            }
            LocalImportMode::ImportFile => {
                let weak_browse = cx.weak_entity();
                let path_row = div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(8.0))
                    .child(
                        div().flex_1().min_w_0().child(
                            TextField::outlined(
                                "local-profile-path",
                                rust_i18n::t!("profiles.local_sheet.file_path_label").into_owned(),
                                &self.file_path_input,
                            )
                            .clearable(true)
                            .required(true),
                        ),
                    )
                    .child(
                        div().pt(px(8.0)).flex_none().child(
                            Button::new(
                                "local-profile-browse-btn",
                                rust_i18n::t!("profiles.local_sheet.browse_btn").into_owned(),
                            )
                            .variant(ButtonVariant::Outlined)
                            .leading_icon(symbol_icon(Symbol::FOLDER_OPEN))
                            .on_activate(move |window, cx| {
                                let _ = weak_browse.update(cx, |this, cx| {
                                    this.browse_local_file(window, cx);
                                });
                            }),
                        ),
                    );
                body = body.child(path_row);
            }
        }

        let cancel_shell = self.shell.clone();
        let weak_submit = cx.weak_entity();
        let (submit_label, submit_icon) = match self.mode {
            LocalImportMode::CreateNew => (
                rust_i18n::t!("common.create").into_owned(),
                symbol_icon(Symbol::ADD),
            ),
            LocalImportMode::ImportFile => (
                rust_i18n::t!("common.import").into_owned(),
                symbol_icon(Symbol::UPLOAD_FILE),
            ),
        };

        let footer = div()
            .w_full()
            .pt(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .justify_end()
            .gap(px(8.0))
            .child(
                Button::new(
                    "local-profile-cancel-btn",
                    rust_i18n::t!("common.cancel").into_owned(),
                )
                .variant(ButtonVariant::Text)
                .on_activate(move |window, cx| {
                    cancel_shell.close_sheet(window, cx);
                }),
            )
            .child(
                Button::new("local-profile-submit-btn", submit_label)
                    .variant(ButtonVariant::Filled)
                    .leading_icon(submit_icon)
                    .on_activate(move |window, cx| {
                        let _ = weak_submit.update(cx, |this, cx| {
                            this.submit(window, cx);
                        });
                    }),
            );

        body.child(footer)
    }
}
