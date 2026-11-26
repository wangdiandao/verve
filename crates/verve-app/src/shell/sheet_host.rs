//! Generic [`SideSheetHost`], [`ConfirmDialogHost`], and [`ShellHandle`] avoiding monolithic modal enums.

use std::rc::Rc;

use gpui::{
    AnyView, App, AppContext, Context, Entity, IntoElement, Pixels, Render, SharedString,
    WeakEntity, Window, div, px,
};
use gpui_material::{
    Dialog, DialogState, SemanticTone, Sheet, SheetEdge, SheetMode, SheetPlacement,
};

use super::nav::RouteId;

/// Descriptor submitted by any feature module to present a right-hand slide-over [`Sheet`].
#[derive(Clone)]
pub struct SideSheetSpec {
    /// Accessible title rendered in the sheet header.
    pub title: SharedString,
    /// Arbitrary self-contained GPUI view entity rendered inside the sheet body.
    pub view: AnyView,
    /// Optional width override in logical pixels (defaults to `380px`).
    pub width: Option<Pixels>,
}

impl SideSheetSpec {
    /// Creates a new [`SideSheetSpec`] wrapping `view`.
    pub fn new(title: impl Into<SharedString>, view: AnyView) -> Self {
        Self {
            title: title.into(),
            view,
            width: None,
        }
    }

    /// Sets an explicit sheet width in logical pixels.
    pub fn width(mut self, width: Pixels) -> Self {
        self.width = Some(width);
        self
    }
}

/// Callback invoked when a confirmation dialog is accepted.
pub type ConfirmCallback = Rc<dyn Fn(&mut Window, &mut App)>;

/// Callback invoked when a feature slice requests shell route navigation.
pub type NavigateCallback = Rc<dyn Fn(RouteId, &mut Window, &mut App)>;

/// Descriptor submitted by any feature module to present a lightweight confirmation [`Dialog`].
#[derive(Clone)]
pub struct ConfirmDialogSpec {
    /// Dialog heading.
    pub title: SharedString,
    /// Supporting explanation text.
    pub description: SharedString,
    /// Confirm button label.
    pub confirm_label: SharedString,
    /// Cancel button label.
    pub cancel_label: SharedString,
    /// Styles the confirm action with `SemanticTone::Danger` when `true`.
    pub destructive: bool,
    /// Callback invoked when the user confirms.
    pub on_confirm: ConfirmCallback,
}

impl ConfirmDialogSpec {
    /// Creates a confirmation dialog specification.
    pub fn new(
        title: impl Into<SharedString>,
        description: impl Into<SharedString>,
        confirm_label: impl Into<SharedString>,
        on_confirm: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            title: title.into(),
            description: description.into(),
            confirm_label: confirm_label.into(),
            cancel_label: rust_i18n::t!("common.cancel").into_owned().into(),
            destructive: false,
            on_confirm: Rc::new(on_confirm),
        }
    }

    /// Marks the confirmation action as destructive (tinted with error tone).
    pub fn destructive(mut self, destructive: bool) -> Self {
        self.destructive = destructive;
        self
    }

    /// Overrides the cancel button label (defaults to `"Cancel"`).
    pub fn cancel_label(mut self, label: impl Into<SharedString>) -> Self {
        self.cancel_label = label.into();
        self
    }
}

/// Generic right-hand side sheet host retaining no feature-specific enums or state.
pub struct SideSheetHost {
    dialog_state: Entity<DialogState>,
    active: Option<SideSheetSpec>,
}

impl SideSheetHost {
    /// Creates a closed [`SideSheetHost`].
    pub fn new(cx: &mut Context<Self>) -> Self {
        let dialog_state = cx.new(DialogState::new);
        Self {
            dialog_state,
            active: None,
        }
    }

    /// Opens the side sheet displaying `spec`.
    pub fn open(&mut self, spec: SideSheetSpec, window: &mut Window, cx: &mut Context<Self>) {
        self.active = Some(spec);
        self.dialog_state.update(cx, |state, cx| {
            state.open(window, cx);
        });
        cx.notify();
    }

    /// Programmatically closes the active side sheet and drops its view.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog_state.update(cx, |state, cx| {
            state.close(window, cx);
        });
        self.active = None;
        cx.notify();
    }

    /// Returns `true` when a side sheet is currently open.
    pub fn is_open(&self, cx: &App) -> bool {
        self.active.is_some() && self.dialog_state.read(cx).is_open()
    }
}

impl Render for SideSheetHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(spec) = self.active.as_ref() else {
            return div().into_any_element();
        };

        let weak = cx.weak_entity();
        let width = spec.width.unwrap_or(px(380.0));
        Sheet::new(
            "verve-side-sheet",
            &self.dialog_state,
            spec.title.clone(),
            spec.view.clone(),
        )
        .edge(SheetEdge::End)
        .mode(SheetMode::Modal)
        .placement(SheetPlacement::Docked)
        .extent(width)
        .close_label(rust_i18n::t!("common.close").into_owned())
        .on_closed(move |_, _, cx| {
            let _ = weak.update(cx, |host, cx| {
                host.active = None;
                cx.notify();
            });
        })
        .into_any_element()
    }
}

/// Generic confirmation dialog host retaining no feature-specific enums or state.
pub struct ConfirmDialogHost {
    dialog_state: Entity<DialogState>,
    active: Option<ConfirmDialogSpec>,
}

impl ConfirmDialogHost {
    /// Creates a closed [`ConfirmDialogHost`].
    pub fn new(cx: &mut Context<Self>) -> Self {
        let dialog_state = cx.new(DialogState::new);
        Self {
            dialog_state,
            active: None,
        }
    }

    /// Opens the confirmation dialog described by `spec`.
    pub fn open(&mut self, spec: ConfirmDialogSpec, window: &mut Window, cx: &mut Context<Self>) {
        self.active = Some(spec);
        self.dialog_state.update(cx, |state, cx| {
            state.open(window, cx);
        });
        cx.notify();
    }

    /// Programmatically closes the active confirmation dialog.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog_state.update(cx, |state, cx| {
            state.close(window, cx);
        });
        self.active = None;
        cx.notify();
    }

    /// Returns `true` when a confirmation dialog is currently open.
    pub fn is_open(&self, cx: &App) -> bool {
        self.active.is_some() && self.dialog_state.read(cx).is_open()
    }
}

impl Render for ConfirmDialogHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(spec) = self.active.as_ref() else {
            return div().into_any_element();
        };

        let weak = cx.weak_entity();
        let on_confirm = Rc::clone(&spec.on_confirm);
        let tone = if spec.destructive {
            SemanticTone::Danger
        } else {
            SemanticTone::Default
        };

        Dialog::new(
            "verve-confirm-dialog",
            &self.dialog_state,
            spec.title.clone(),
        )
        .description(spec.description.clone())
        .cancel_label(spec.cancel_label.clone())
        .confirm_tone(tone)
        .on_confirm(spec.confirm_label.clone(), move |window, cx| {
            on_confirm(window, cx);
        })
        .on_closed(move |_, _, cx| {
            let _ = weak.update(cx, |host, cx| {
                host.active = None;
                cx.notify();
            });
        })
        .into_any_element()
    }
}

/// Capability handle passed to vertical feature slices for opening scoped sheets,
/// confirmation dialogs, and triggering shell navigation.
#[derive(Clone)]
pub struct ShellHandle {
    sheet_host: WeakEntity<SideSheetHost>,
    confirm_host: WeakEntity<ConfirmDialogHost>,
    on_navigate: NavigateCallback,
}

impl ShellHandle {
    pub(crate) fn new(
        sheet_host: WeakEntity<SideSheetHost>,
        confirm_host: WeakEntity<ConfirmDialogHost>,
        on_navigate: impl Fn(RouteId, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            sheet_host,
            confirm_host,
            on_navigate: Rc::new(on_navigate),
        }
    }

    /// Opens a feature-owned view inside the shell's right-hand [`SideSheetHost`].
    pub fn open_sheet(&self, spec: SideSheetSpec, window: &mut Window, cx: &mut App) {
        let _ = self.sheet_host.update(cx, |host, cx| {
            host.open(spec, window, cx);
        });
    }

    /// Closes the currently open side sheet, if any.
    pub fn close_sheet(&self, window: &mut Window, cx: &mut App) {
        let _ = self.sheet_host.update(cx, |host, cx| {
            host.close(window, cx);
        });
    }

    /// Opens a confirmation dialog inside the shell's [`ConfirmDialogHost`].
    pub fn open_confirm(&self, spec: ConfirmDialogSpec, window: &mut Window, cx: &mut App) {
        let _ = self.confirm_host.update(cx, |host, cx| {
            host.open(spec, window, cx);
        });
    }

    /// Closes the currently open confirmation dialog, if any.
    pub fn close_confirm(&self, window: &mut Window, cx: &mut App) {
        let _ = self.confirm_host.update(cx, |host, cx| {
            host.close(window, cx);
        });
    }

    /// Navigates the main window to `route`.
    pub fn navigate(&self, route: RouteId, window: &mut Window, cx: &mut App) {
        (self.on_navigate)(route, window, cx);
    }
}
