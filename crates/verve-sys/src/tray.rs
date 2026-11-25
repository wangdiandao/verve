//! Cross-platform system tray adapter, procedural RGBA icon renderer, and headless test backend.

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::SysError;

/// Visual state represented by the system tray icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum TrayIconState {
    /// Kernel idle or proxy takeover disabled.
    #[default]
    Idle,
    /// System proxy takeover active.
    SystemProxy,
    /// TUN virtual interface active.
    Tun,
}

/// Snapshot of UI and proxy runtime state reflected by the tray menu and icon.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TrayMenuState {
    /// Whether the main application window is currently visible.
    pub window_visible: bool,
    /// Active proxy mode label (`"rule"`, `"global"`, `"direct"`).
    pub proxy_mode: String,
    /// Whether OS system proxy takeover is enabled.
    pub system_proxy_enabled: bool,
    /// Whether TUN mode is enabled.
    pub tun_enabled: bool,
}

impl TrayMenuState {
    /// Derives the corresponding [`TrayIconState`] from the active proxy/TUN flags.
    pub fn icon_state(&self) -> TrayIconState {
        if self.tun_enabled {
            TrayIconState::Tun
        } else if self.system_proxy_enabled {
            TrayIconState::SystemProxy
        } else {
            TrayIconState::Idle
        }
    }
}

/// User actions emitted by tray clicks and tray context menu items.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrayAction {
    /// Toggle main application window visibility.
    ToggleWindow,
    /// Show and focus the main application window.
    ShowWindow,
    /// Switch the kernel routing mode (`"rule"`, `"global"`, `"direct"`).
    SetProxyMode(String),
    /// Toggle OS system proxy takeover.
    ToggleSystemProxy,
    /// Toggle TUN virtual interface mode.
    ToggleTun,
    /// Exit the application cleanly.
    Quit,
}

/// Pure-Rust procedural anti-aliased RGBA badge renderer for the system tray icon.
///
/// Produces `size * size * 4` RGBA bytes with distinct foreground and accent colors per [`TrayIconState`].
pub fn render_tray_icon_rgba(state: TrayIconState, size: u32) -> Vec<u8> {
    if size == 0 {
        return Vec::new();
    }

    let (outer_rgb, inner_rgb) = match state {
        TrayIconState::Idle => ([115u8, 125, 140], [215u8, 222, 232]),
        TrayIconState::SystemProxy => ([16u8, 163, 127], [224u8, 252, 244]),
        TrayIconState::Tun => ([59u8, 130, 246], [224u8, 240, 255]),
    };

    let dim = size as usize;
    let mut rgba = vec![0u8; dim * dim * 4];
    let center = (size as f32 - 1.0) * 0.5;
    let outer_radius = (size as f32) * 0.43;
    let inner_radius = (size as f32) * 0.17;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();

            let outer_cov = (outer_radius + 0.5 - dist).clamp(0.0, 1.0);
            if outer_cov <= 0.0 {
                continue;
            }

            let inner_cov = (inner_radius + 0.5 - dist).clamp(0.0, 1.0);
            let r = lerp_channel(outer_rgb[0], inner_rgb[0], inner_cov);
            let g = lerp_channel(outer_rgb[1], inner_rgb[1], inner_cov);
            let b = lerp_channel(outer_rgb[2], inner_rgb[2], inner_cov);
            let a = (outer_cov * 255.0).round() as u8;

            let idx = ((y as usize) * dim + (x as usize)) * 4;
            rgba[idx] = r;
            rgba[idx + 1] = g;
            rgba[idx + 2] = b;
            rgba[idx + 3] = a;
        }
    }

    rgba
}

fn lerp_channel(a: u8, b: u8, t: f32) -> u8 {
    let mixed = (a as f32) * (1.0 - t) + (b as f32) * t;
    mixed.round().clamp(0.0, 255.0) as u8
}

/// Platform-independent tray backend interface.
pub trait TrayBackend: Send {
    /// Updates the tray icon and context menu to reflect `state`.
    fn update_state(&mut self, state: &TrayMenuState) -> Result<(), SysError>;
}

/// Headless in-memory [`TrayBackend`] recording state transitions and injecting test actions.
#[derive(Debug)]
pub struct HeadlessTrayBackend {
    history: Vec<TrayMenuState>,
    action_tx: mpsc::UnboundedSender<TrayAction>,
}

impl HeadlessTrayBackend {
    /// Creates a headless tray backend together with the receiving end of its action channel.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<TrayAction>) {
        let (action_tx, action_rx) = mpsc::unbounded_channel();
        (Self::with_sender(action_tx), action_rx)
    }

    /// Creates a headless tray backend forwarding injected actions into `action_tx`.
    pub fn with_sender(action_tx: mpsc::UnboundedSender<TrayAction>) -> Self {
        Self {
            history: Vec::new(),
            action_tx,
        }
    }

    /// Injects a simulated user [`TrayAction`] into the action receiver channel.
    pub fn emit_action(&self, action: TrayAction) -> Result<(), SysError> {
        self.action_tx
            .send(action)
            .map_err(|_| SysError::Tray("tray action receiver dropped".into()))
    }

    /// Returns all [`TrayMenuState`] snapshots passed to [`TrayBackend::update_state`].
    pub fn history(&self) -> &[TrayMenuState] {
        &self.history
    }

    /// Returns the most recently applied [`TrayMenuState`], if any.
    pub fn latest_state(&self) -> Option<&TrayMenuState> {
        self.history.last()
    }
}

impl TrayBackend for HeadlessTrayBackend {
    fn update_state(&mut self, state: &TrayMenuState) -> Result<(), SysError> {
        self.history.push(state.clone());
        Ok(())
    }
}

/// Constructs the default platform tray backend, falling back to [`HeadlessTrayBackend`]
/// when the native desktop tray service (such as Linux session D-Bus) is unavailable.
pub fn create_platform_tray(action_tx: mpsc::UnboundedSender<TrayAction>) -> Box<dyn TrayBackend> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        match NativeTray::new(action_tx.clone()) {
            Ok(tray) => Box::new(tray),
            Err(err) => {
                tracing::warn!(error = %err, "native tray unavailable; falling back to headless tray");
                Box::new(HeadlessTrayBackend::with_sender(action_tx))
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        match LinuxKsniTray::try_new(action_tx.clone()) {
            Ok(tray) => Box::new(tray),
            Err(err) => {
                tracing::warn!(error = %err, "StatusNotifierItem D-Bus unavailable; falling back to headless tray");
                Box::new(HeadlessTrayBackend::with_sender(action_tx))
            }
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        Box::new(HeadlessTrayBackend::with_sender(action_tx))
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod native_desktop {
    use std::sync::{Mutex, OnceLock};

    use tokio::sync::mpsc;
    use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    use super::{TrayAction, TrayIconState, TrayMenuState, render_tray_icon_rgba};
    use crate::SysError;

    const ICON_SIZE: u32 = 32;
    const ID_TOGGLE_WINDOW: &str = "verve.tray.toggle_window";
    const ID_MODE_RULE: &str = "verve.tray.mode.rule";
    const ID_MODE_GLOBAL: &str = "verve.tray.mode.global";
    const ID_MODE_DIRECT: &str = "verve.tray.mode.direct";
    const ID_SYSTEM_PROXY: &str = "verve.tray.system_proxy";
    const ID_TUN: &str = "verve.tray.tun";
    const ID_QUIT: &str = "verve.tray.quit";

    static ACTIVE_ACTION_TX: OnceLock<Mutex<Option<mpsc::UnboundedSender<TrayAction>>>> =
        OnceLock::new();
    static HANDLERS_INSTALLED: OnceLock<()> = OnceLock::new();

    fn install_global_handlers(action_tx: mpsc::UnboundedSender<TrayAction>) {
        let slot = ACTIVE_ACTION_TX.get_or_init(|| Mutex::new(None));
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(action_tx);
        }

        HANDLERS_INSTALLED.get_or_init(|| {
            MenuEvent::set_event_handler(Some(|event: MenuEvent| {
                let action = match event.id().as_ref() {
                    ID_TOGGLE_WINDOW => Some(TrayAction::ToggleWindow),
                    ID_MODE_RULE => Some(TrayAction::SetProxyMode("rule".into())),
                    ID_MODE_GLOBAL => Some(TrayAction::SetProxyMode("global".into())),
                    ID_MODE_DIRECT => Some(TrayAction::SetProxyMode("direct".into())),
                    ID_SYSTEM_PROXY => Some(TrayAction::ToggleSystemProxy),
                    ID_TUN => Some(TrayAction::ToggleTun),
                    ID_QUIT => Some(TrayAction::Quit),
                    _ => None,
                };
                if let Some(action) = action
                    && let Some(slot) = ACTIVE_ACTION_TX.get()
                    && let Ok(guard) = slot.lock()
                    && let Some(tx) = guard.as_ref()
                {
                    let _ = tx.send(action);
                }
            }));

            TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                    && let Some(slot) = ACTIVE_ACTION_TX.get()
                    && let Ok(guard) = slot.lock()
                    && let Some(tx) = guard.as_ref()
                {
                    let _ = tx.send(TrayAction::ToggleWindow);
                }
            }));
        });
    }

    fn build_tray_objects() -> Result<TrayObjects, SysError> {
        let toggle_window = MenuItem::with_id(ID_TOGGLE_WINDOW, "Show / Hide Verve", true, None);
        let mode_rule = CheckMenuItem::with_id(ID_MODE_RULE, "Mode: Rule", true, true, None);
        let mode_global = CheckMenuItem::with_id(ID_MODE_GLOBAL, "Mode: Global", true, false, None);
        let mode_direct = CheckMenuItem::with_id(ID_MODE_DIRECT, "Mode: Direct", true, false, None);
        let system_proxy =
            CheckMenuItem::with_id(ID_SYSTEM_PROXY, "System Proxy", true, false, None);
        let tun = CheckMenuItem::with_id(ID_TUN, "TUN Mode", true, false, None);
        let quit = MenuItem::with_id(ID_QUIT, "Quit", true, None);

        let menu = Menu::new();
        menu.append_items(&[
            &toggle_window,
            &PredefinedMenuItem::separator(),
            &mode_rule,
            &mode_global,
            &mode_direct,
            &PredefinedMenuItem::separator(),
            &system_proxy,
            &tun,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(|err| SysError::Tray(err.to_string()))?;

        let icon = Icon::from_rgba(
            render_tray_icon_rgba(TrayIconState::Idle, ICON_SIZE),
            ICON_SIZE,
            ICON_SIZE,
        )
        .map_err(|err| SysError::Tray(err.to_string()))?;

        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .with_tooltip("Verve")
            .with_menu_on_left_click(false)
            .build()
            .map_err(|err| SysError::Tray(err.to_string()))?;

        Ok(TrayObjects {
            tray_icon,
            toggle_window,
            mode_rule,
            mode_global,
            mode_direct,
            system_proxy,
            tun,
        })
    }

    struct TrayObjects {
        tray_icon: TrayIcon,
        toggle_window: MenuItem,
        mode_rule: CheckMenuItem,
        mode_global: CheckMenuItem,
        mode_direct: CheckMenuItem,
        system_proxy: CheckMenuItem,
        tun: CheckMenuItem,
    }

    impl TrayObjects {
        fn apply_state(&self, state: &TrayMenuState) -> Result<(), SysError> {
            self.toggle_window.set_text(if state.window_visible {
                "Hide Verve"
            } else {
                "Show Verve"
            });
            self.mode_rule
                .set_checked(state.proxy_mode.eq_ignore_ascii_case("rule"));
            self.mode_global
                .set_checked(state.proxy_mode.eq_ignore_ascii_case("global"));
            self.mode_direct
                .set_checked(state.proxy_mode.eq_ignore_ascii_case("direct"));
            self.system_proxy.set_checked(state.system_proxy_enabled);
            self.tun.set_checked(state.tun_enabled);

            let icon = Icon::from_rgba(
                render_tray_icon_rgba(state.icon_state(), ICON_SIZE),
                ICON_SIZE,
                ICON_SIZE,
            )
            .map_err(|err| SysError::Tray(err.to_string()))?;
            self.tray_icon
                .set_icon(Some(icon))
                .map_err(|err| SysError::Tray(err.to_string()))?;

            let tooltip = format!(
                "Verve ({}{}{})",
                state.proxy_mode,
                if state.system_proxy_enabled {
                    ", SysProxy"
                } else {
                    ""
                },
                if state.tun_enabled { ", TUN" } else { "" }
            );
            let _ = self.tray_icon.set_tooltip(Some(tooltip));
            Ok(())
        }
    }

    #[cfg(target_os = "windows")]
    mod win32 {
        use std::collections::VecDeque;
        use std::sync::{Arc, Mutex};
        use std::thread::JoinHandle;

        use tokio::sync::mpsc;
        use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
        use windows_sys::Win32::System::Threading::GetCurrentThreadId;

        use super::{TrayMenuState, build_tray_objects, install_global_handlers};
        use crate::SysError;
        use crate::tray::{TrayAction, TrayBackend};

        const WM_APP_UPDATE: u32 = 0x8001;
        const WM_APP_QUIT: u32 = 0x8002;
        const PM_NOREMOVE: u32 = 0x0000;

        #[repr(C)]
        #[allow(non_snake_case)]
        struct WinMsg {
            hwnd: HWND,
            message: u32,
            wParam: WPARAM,
            lParam: LPARAM,
            time: u32,
            pt: POINT,
        }

        #[link(name = "user32")]
        unsafe extern "system" {
            fn PeekMessageW(
                lpmsg: *mut WinMsg,
                hwnd: HWND,
                wmsgfiltermin: u32,
                wmsgfiltermax: u32,
                wremovemsg: u32,
            ) -> i32;
            fn GetMessageW(
                lpmsg: *mut WinMsg,
                hwnd: HWND,
                wmsgfiltermin: u32,
                wmsgfiltermax: u32,
            ) -> i32;
            fn TranslateMessage(lpmsg: *const WinMsg) -> i32;
            fn DispatchMessageW(lpmsg: *const WinMsg) -> isize;
            fn PostThreadMessageW(idthread: u32, msg: u32, wparam: WPARAM, lparam: LPARAM) -> i32;
        }

        /// Native Windows system tray adapter backed by a dedicated Win32 message-pump thread.
        pub struct NativeTray {
            thread_id: u32,
            pending_states: Arc<Mutex<VecDeque<TrayMenuState>>>,
            worker: Option<JoinHandle<()>>,
        }

        impl std::fmt::Debug for NativeTray {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct("NativeTray")
                    .field("thread_id", &self.thread_id)
                    .finish_non_exhaustive()
            }
        }

        impl NativeTray {
            /// Creates and registers a native Windows system tray icon and menu.
            pub fn new(action_tx: mpsc::UnboundedSender<TrayAction>) -> Result<Self, SysError> {
                install_global_handlers(action_tx);
                let pending_states = Arc::new(Mutex::new(VecDeque::new()));
                let pending_worker = Arc::clone(&pending_states);
                let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<Result<u32, SysError>>(1);

                let worker = std::thread::Builder::new()
                    .name("verve-tray-win32".into())
                    .spawn(move || {
                        // SAFETY: Calling Win32 thread ID & PeekMessageW to establish a message queue.
                        let mut msg: WinMsg = unsafe { std::mem::zeroed() };
                        unsafe {
                            PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
                        }
                        let tid = unsafe { GetCurrentThreadId() };

                        let objects = match build_tray_objects() {
                            Ok(obj) => {
                                let _ = init_tx.send(Ok(tid));
                                obj
                            }
                            Err(err) => {
                                let _ = init_tx.send(Err(err));
                                return;
                            }
                        };

                        // SAFETY: Standard Win32 message loop running on the tray owner thread.
                        while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
                            if msg.message == WM_APP_QUIT {
                                break;
                            }
                            if msg.message == WM_APP_UPDATE {
                                let drained: Vec<TrayMenuState> = {
                                    let mut guard = pending_worker
                                        .lock()
                                        .expect("tray state queue mutex poisoned");
                                    guard.drain(..).collect()
                                };
                                if let Some(latest) = drained.last()
                                    && let Err(err) = objects.apply_state(latest)
                                {
                                    tracing::warn!(error = %err, "failed to update Windows tray state");
                                }
                                continue;
                            }
                            unsafe {
                                TranslateMessage(&msg);
                                DispatchMessageW(&msg);
                            }
                        }
                    })?;

                let thread_id = init_rx.recv().map_err(|_| {
                    SysError::Tray("tray thread terminated before initialization".into())
                })??;

                Ok(Self {
                    thread_id,
                    pending_states,
                    worker: Some(worker),
                })
            }
        }

        impl TrayBackend for NativeTray {
            fn update_state(&mut self, state: &TrayMenuState) -> Result<(), SysError> {
                {
                    let mut guard = self
                        .pending_states
                        .lock()
                        .expect("tray state queue mutex poisoned");
                    guard.push_back(state.clone());
                }
                // SAFETY: Posting WM_APP_UPDATE to the live tray worker thread ID.
                let ok = unsafe { PostThreadMessageW(self.thread_id, WM_APP_UPDATE, 0, 0) };
                if ok == 0 {
                    return Err(SysError::Io(std::io::Error::last_os_error()));
                }
                Ok(())
            }
        }

        impl Drop for NativeTray {
            fn drop(&mut self) {
                // SAFETY: Signaling the tray message loop thread to exit cleanly.
                unsafe {
                    PostThreadMessageW(self.thread_id, WM_APP_QUIT, 0, 0);
                }
                if let Some(worker) = self.worker.take() {
                    let _ = worker.join();
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    mod macos {
        use std::thread::ThreadId;

        use tokio::sync::mpsc;

        use super::{TrayMenuState, TrayObjects, build_tray_objects, install_global_handlers};
        use crate::SysError;
        use crate::tray::{TrayAction, TrayBackend};

        /// Native macOS status bar tray adapter. Must be created and updated on the main thread.
        pub struct NativeTray {
            objects: TrayObjects,
            owner_thread: ThreadId,
        }

        // SAFETY: Runtime checks enforce all access/mutation occurs only on `owner_thread` (main thread).
        unsafe impl Send for NativeTray {}

        impl std::fmt::Debug for NativeTray {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct("NativeTray")
                    .field("owner_thread", &self.owner_thread)
                    .finish_non_exhaustive()
            }
        }

        impl NativeTray {
            /// Creates the macOS status item tray. Must be invoked on the main thread.
            pub fn new(action_tx: mpsc::UnboundedSender<TrayAction>) -> Result<Self, SysError> {
                install_global_handlers(action_tx);
                let objects = build_tray_objects()?;
                Ok(Self {
                    objects,
                    owner_thread: std::thread::current().id(),
                })
            }
        }

        impl TrayBackend for NativeTray {
            fn update_state(&mut self, state: &TrayMenuState) -> Result<(), SysError> {
                if std::thread::current().id() != self.owner_thread {
                    return Err(SysError::Tray(
                        "macOS NativeTray must be updated from its main owner thread".into(),
                    ));
                }
                self.objects.apply_state(state)
            }
        }
    }

    #[cfg(target_os = "macos")]
    pub use macos::NativeTray;
    #[cfg(target_os = "windows")]
    pub use win32::NativeTray;
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub use native_desktop::NativeTray;

#[cfg(target_os = "linux")]
mod linux_ksni {
    use std::thread::JoinHandle;

    use ksni::TrayMethods;
    use tokio::sync::mpsc;

    use super::{TrayAction, TrayBackend, TrayMenuState, render_tray_icon_rgba};
    use crate::SysError;

    const ICON_SIZE: u32 = 32;

    enum WorkerCmd {
        Update(TrayMenuState),
        Shutdown,
    }

    struct KsniState {
        state: TrayMenuState,
        action_tx: mpsc::UnboundedSender<TrayAction>,
    }

    impl KsniState {
        fn mode_index(&self) -> usize {
            if self.state.proxy_mode.eq_ignore_ascii_case("global") {
                1
            } else if self.state.proxy_mode.eq_ignore_ascii_case("direct") {
                2
            } else {
                0
            }
        }
    }

    impl ksni::Tray for KsniState {
        fn id(&self) -> String {
            "verve".into()
        }

        fn title(&self) -> String {
            "Verve".into()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            let mut data = render_tray_icon_rgba(self.state.icon_state(), ICON_SIZE);
            for chunk in data.as_chunks_mut::<4>().0 {
                chunk.rotate_right(1);
            }
            vec![ksni::Icon {
                width: ICON_SIZE as i32,
                height: ICON_SIZE as i32,
                data,
            }]
        }

        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                title: "Verve".into(),
                description: format!("Mode: {}", self.state.proxy_mode),
                ..Default::default()
            }
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            let _ = self.action_tx.send(TrayAction::ToggleWindow);
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::*;

            vec![
                StandardItem {
                    label: if self.state.window_visible {
                        "Hide Verve".into()
                    } else {
                        "Show Verve".into()
                    },
                    activate: Box::new(|this: &mut Self| {
                        let _ = this.action_tx.send(TrayAction::ToggleWindow);
                    }),
                    ..Default::default()
                }
                .into(),
                ksni::MenuItem::Separator,
                RadioGroup {
                    selected: self.mode_index(),
                    select: Box::new(|this: &mut Self, idx| {
                        let mode = match idx {
                            1 => "global",
                            2 => "direct",
                            _ => "rule",
                        };
                        let _ = this
                            .action_tx
                            .send(TrayAction::SetProxyMode(mode.to_owned()));
                    }),
                    options: vec![
                        RadioItem {
                            label: "Mode: Rule".into(),
                            ..Default::default()
                        },
                        RadioItem {
                            label: "Mode: Global".into(),
                            ..Default::default()
                        },
                        RadioItem {
                            label: "Mode: Direct".into(),
                            ..Default::default()
                        },
                    ],
                }
                .into(),
                ksni::MenuItem::Separator,
                CheckmarkItem {
                    label: "System Proxy".into(),
                    checked: self.state.system_proxy_enabled,
                    activate: Box::new(|this: &mut Self| {
                        let _ = this.action_tx.send(TrayAction::ToggleSystemProxy);
                    }),
                    ..Default::default()
                }
                .into(),
                CheckmarkItem {
                    label: "TUN Mode".into(),
                    checked: self.state.tun_enabled,
                    activate: Box::new(|this: &mut Self| {
                        let _ = this.action_tx.send(TrayAction::ToggleTun);
                    }),
                    ..Default::default()
                }
                .into(),
                ksni::MenuItem::Separator,
                StandardItem {
                    label: "Quit".into(),
                    icon_name: "application-exit".into(),
                    activate: Box::new(|this: &mut Self| {
                        let _ = this.action_tx.send(TrayAction::Quit);
                    }),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    /// Linux `StatusNotifierItem` (`ksni`) tray adapter running over session D-Bus without GTK.
    pub struct LinuxKsniTray {
        cmd_tx: mpsc::UnboundedSender<WorkerCmd>,
        worker: Option<JoinHandle<()>>,
    }

    impl std::fmt::Debug for LinuxKsniTray {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("LinuxKsniTray").finish_non_exhaustive()
        }
    }

    impl LinuxKsniTray {
        /// Attempts to register the tray icon on the Linux session D-Bus.
        pub fn try_new(action_tx: mpsc::UnboundedSender<TrayAction>) -> Result<Self, SysError> {
            let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<WorkerCmd>();
            let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<Result<(), SysError>>(1);

            let worker = std::thread::Builder::new()
                .name("verve-tray-ksni".into())
                .spawn(move || {
                    let rt = match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(rt) => rt,
                        Err(err) => {
                            let _ = init_tx.send(Err(SysError::Io(err)));
                            return;
                        }
                    };

                    rt.block_on(async move {
                        let tray = KsniState {
                            state: TrayMenuState {
                                proxy_mode: "rule".into(),
                                ..Default::default()
                            },
                            action_tx,
                        };
                        let handle = match tray.spawn().await {
                            Ok(handle) => {
                                let _ = init_tx.send(Ok(()));
                                handle
                            }
                            Err(err) => {
                                let _ = init_tx.send(Err(SysError::Tray(err.to_string())));
                                return;
                            }
                        };

                        while let Some(cmd) = cmd_rx.recv().await {
                            match cmd {
                                WorkerCmd::Update(next_state) => {
                                    let _ = handle
                                        .update(move |tray: &mut KsniState| {
                                            tray.state = next_state;
                                        })
                                        .await;
                                }
                                WorkerCmd::Shutdown => break,
                            }
                        }

                        handle.shutdown().await;
                    });
                })?;

            init_rx
                .recv()
                .map_err(|_| SysError::Tray("ksni tray worker exited during startup".into()))??;

            Ok(Self {
                cmd_tx,
                worker: Some(worker),
            })
        }
    }

    impl TrayBackend for LinuxKsniTray {
        fn update_state(&mut self, state: &TrayMenuState) -> Result<(), SysError> {
            self.cmd_tx
                .send(WorkerCmd::Update(state.clone()))
                .map_err(|_| SysError::Tray("ksni tray worker is no longer running".into()))
        }
    }

    impl Drop for LinuxKsniTray {
        fn drop(&mut self) {
            let _ = self.cmd_tx.send(WorkerCmd::Shutdown);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux_ksni::LinuxKsniTray;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_tray_icon_rgba_produces_distinct_valid_buffers_for_all_states() {
        let idle = render_tray_icon_rgba(TrayIconState::Idle, 32);
        let sys_proxy = render_tray_icon_rgba(TrayIconState::SystemProxy, 32);
        let tun = render_tray_icon_rgba(TrayIconState::Tun, 32);

        assert_eq!(idle.len(), 32 * 32 * 4);
        assert_eq!(sys_proxy.len(), 32 * 32 * 4);
        assert_eq!(tun.len(), 32 * 32 * 4);

        assert_ne!(idle, sys_proxy);
        assert_ne!(sys_proxy, tun);
        assert_ne!(idle, tun);

        // Center pixel must be opaque across all states; corner pixel must be transparent.
        let center_alpha_idx = (16 * 32 + 16) * 4 + 3;
        assert_eq!(idle[center_alpha_idx], 255);
        assert_eq!(sys_proxy[center_alpha_idx], 255);
        assert_eq!(tun[center_alpha_idx], 255);
        assert_eq!(idle[3], 0);

        assert!(render_tray_icon_rgba(TrayIconState::Idle, 0).is_empty());
    }

    #[tokio::test]
    async fn headless_tray_backend_records_states_and_emits_actions() {
        let (mut backend, mut rx) = HeadlessTrayBackend::new();

        let state1 = TrayMenuState {
            window_visible: true,
            proxy_mode: "rule".into(),
            system_proxy_enabled: true,
            tun_enabled: false,
        };
        assert_eq!(state1.icon_state(), TrayIconState::SystemProxy);
        backend.update_state(&state1).unwrap();

        let state2 = TrayMenuState {
            window_visible: false,
            proxy_mode: "global".into(),
            system_proxy_enabled: true,
            tun_enabled: true,
        };
        assert_eq!(state2.icon_state(), TrayIconState::Tun);
        backend.update_state(&state2).unwrap();

        assert_eq!(backend.history(), &[state1, state2.clone()]);
        assert_eq!(backend.latest_state(), Some(&state2));

        backend.emit_action(TrayAction::ToggleWindow).unwrap();
        backend
            .emit_action(TrayAction::SetProxyMode("direct".into()))
            .unwrap();

        assert_eq!(rx.recv().await.unwrap(), TrayAction::ToggleWindow);
        assert_eq!(
            rx.recv().await.unwrap(),
            TrayAction::SetProxyMode("direct".into())
        );
    }

    #[test]
    fn tray_menu_state_derives_idle_when_no_takeover_enabled() {
        let state = TrayMenuState {
            window_visible: true,
            proxy_mode: "rule".into(),
            system_proxy_enabled: false,
            tun_enabled: false,
        };
        assert_eq!(state.icon_state(), TrayIconState::Idle);
    }
}
