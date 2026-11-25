//! Application-level system tray controller living independently of GPUI windows.

use std::rc::Rc;

use gpui::{App, AsyncApp, Context, Entity, Subscription, WeakEntity, WindowHandle};
use gpui_base::Root;
use tokio::sync::mpsc;
use verve_core::DomainKind;
use verve_core::kernel::ProxyMode;
use verve_core::sys::{TrayAction, TrayBackend, TrayMenuState};

use crate::bridge::{CommandDispatcher, DomainChanged, UiStore};

/// Factory opening a fresh main [`crate::shell::AppShell`] window.
pub type WindowOpener = Rc<dyn Fn(&mut App) -> gpui::Result<WindowHandle<Root>>>;

/// Callback invoked when the user triggers Quit from the system tray.
pub type QuitCallback = Rc<dyn Fn(&mut App)>;

/// Application-level tray controller managing menu state, window visibility, and tray actions.
///
/// Survives closing the main GPUI window so Verve stays resident in the system tray and can
/// recreate the window on demand from the latest [`UiStore`] state.
pub struct TrayController {
    backend: Box<dyn TrayBackend>,
    state: TrayMenuState,
    store: Entity<UiStore>,
    commands: CommandDispatcher,
    main_window: Option<WindowHandle<Root>>,
    open_window: WindowOpener,
    on_quit: QuitCallback,
    _store_sub: Subscription,
    _action_task: gpui::Task<()>,
}

impl TrayController {
    /// Creates a new [`TrayController`], binds it to `store` and `action_rx`, and syncs initial state.
    pub fn new(
        backend: Box<dyn TrayBackend>,
        mut action_rx: mpsc::UnboundedReceiver<TrayAction>,
        store: Entity<UiStore>,
        commands: CommandDispatcher,
        open_window: WindowOpener,
        on_quit: Option<QuitCallback>,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial_runtime = store.read(cx).runtime().clone();
        let initial_state = TrayMenuState {
            window_visible: false,
            proxy_mode: proxy_mode_str(initial_runtime.proxy_mode).to_owned(),
            system_proxy_enabled: initial_runtime.system_proxy_enabled,
            tun_enabled: initial_runtime.tun_enabled,
        };

        let store_sub = cx.subscribe(&store, |this, _store, ev: &DomainChanged, cx| {
            if ev.0 == DomainKind::Runtime {
                this.sync_from_store(cx);
            }
        });

        let weak = cx.weak_entity();
        cx.on_window_closed(move |cx, closed_id| {
            let _ = weak.update(cx, |ctrl, cx| {
                ctrl.handle_window_closed(closed_id, cx);
            });
        })
        .detach();

        let action_task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            while let Some(action) = action_rx.recv().await {
                if this
                    .update(cx, |ctrl, cx| ctrl.handle_action(action, cx))
                    .is_err()
                {
                    break;
                }
            }
        });

        let mut controller = Self {
            backend,
            state: initial_state,
            store,
            commands,
            main_window: None,
            open_window,
            on_quit: on_quit.unwrap_or_else(|| Rc::new(|cx| cx.quit())),
            _store_sub: store_sub,
            _action_task: action_task,
        };
        controller.push_tray_state();
        controller
    }

    /// Returns the most recently synced [`TrayMenuState`].
    pub fn menu_state(&self) -> &TrayMenuState {
        &self.state
    }

    /// Returns the currently open main window handle, if any.
    pub fn active_window(&self) -> Option<WindowHandle<Root>> {
        self.main_window
    }

    /// Attaches an externally opened main window and updates `window_visible = true`.
    pub fn attach_window(&mut self, handle: WindowHandle<Root>, cx: &mut Context<Self>) {
        self.main_window = Some(handle);
        self.state.window_visible = true;
        self.push_tray_state();
        cx.notify();
    }

    /// Opens a new main window if none is open, or activates the existing one.
    pub fn show_or_focus_window(
        &mut self,
        cx: &mut Context<Self>,
    ) -> gpui::Result<WindowHandle<Root>> {
        if let Some(handle) = self.main_window
            && cx
                .windows()
                .iter()
                .any(|w| w.window_id() == handle.window_id())
        {
            let _ = handle.update(cx, |_, window, _| {
                window.activate_window();
            });
            if !self.state.window_visible {
                self.state.window_visible = true;
                self.push_tray_state();
            }
            return Ok(handle);
        }

        let handle = (self.open_window)(cx)?;
        self.attach_window(handle, cx);
        Ok(handle)
    }

    /// Closes the main window if currently open, leaving the app resident in the tray.
    pub fn hide_window(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.main_window.take() {
            let _ = handle.update(cx, |_, window, _| {
                window.remove_window();
            });
        }
        self.state.window_visible = false;
        self.push_tray_state();
        cx.notify();
    }

    /// Processes a [`TrayAction`] originating from the platform tray or test harness.
    pub fn handle_action(&mut self, action: TrayAction, cx: &mut Context<Self>) {
        match action {
            TrayAction::ShowWindow => {
                if let Err(err) = self.show_or_focus_window(cx) {
                    tracing::warn!(error = %err, "failed to show main window from tray");
                }
            }
            TrayAction::ToggleWindow => {
                let is_open = self
                    .main_window
                    .is_some_and(|h| cx.windows().iter().any(|w| w.window_id() == h.window_id()));
                if is_open {
                    self.hide_window(cx);
                } else if let Err(err) = self.show_or_focus_window(cx) {
                    tracing::warn!(error = %err, "failed to toggle main window from tray");
                }
            }
            TrayAction::SetProxyMode(mode_str) => {
                let mode = match mode_str.trim().to_ascii_lowercase().as_str() {
                    "global" => ProxyMode::Global,
                    "direct" => ProxyMode::Direct,
                    _ => ProxyMode::Rule,
                };
                self.commands.set_proxy_mode(mode);
            }
            TrayAction::ToggleSystemProxy => {
                let next = !self.store.read(cx).runtime().system_proxy_enabled;
                self.commands.set_system_proxy(next);
            }
            TrayAction::ToggleTun => {
                let next = !self.store.read(cx).runtime().tun_enabled;
                self.commands.set_tun_enabled(next);
            }
            TrayAction::Quit => {
                self.hide_window(cx);
                let commands = self.commands.clone();
                let on_quit = Rc::clone(&self.on_quit);
                cx.spawn(async move |_, cx| {
                    if let Err(err) = commands.shutdown().await {
                        tracing::warn!(error = %err, "coordinator shutdown error during tray quit");
                    }
                    cx.update(|cx| on_quit(cx));
                })
                .detach();
            }
        }
    }

    fn handle_window_closed(&mut self, closed_id: gpui::WindowId, cx: &mut Context<Self>) {
        if self
            .main_window
            .is_some_and(|handle| handle.window_id() == closed_id)
        {
            self.main_window = None;
            self.state.window_visible = false;
            self.push_tray_state();
            cx.notify();
        }
    }

    fn sync_from_store(&mut self, cx: &mut Context<Self>) {
        let runtime = self.store.read(cx).runtime().clone();
        self.state.proxy_mode = proxy_mode_str(runtime.proxy_mode).to_owned();
        self.state.system_proxy_enabled = runtime.system_proxy_enabled;
        self.state.tun_enabled = runtime.tun_enabled;
        self.push_tray_state();
        cx.notify();
    }

    fn push_tray_state(&mut self) {
        if let Err(err) = self.backend.update_state(&self.state) {
            tracing::warn!(error = %err, "failed to update tray state");
        }
    }
}

const fn proxy_mode_str(mode: ProxyMode) -> &'static str {
    match mode {
        ProxyMode::Rule => "rule",
        ProxyMode::Global => "global",
        ProxyMode::Direct => "direct",
    }
}
