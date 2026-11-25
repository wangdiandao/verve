//! Desktop window shell, declarative navigation, scoped sheet/dialog hosts, and tray controller.

pub mod nav;
pub mod sheet_host;
pub mod tray;
pub mod window;

pub use nav::{RouteDescriptor, RouteId, route_registry};
pub use sheet_host::{
    ConfirmCallback, ConfirmDialogHost, ConfirmDialogSpec, NavigateCallback, ShellHandle,
    SideSheetHost, SideSheetSpec,
};
pub use tray::{QuitCallback, TrayController, WindowOpener};
pub use window::{AppShell, default_main_window_options};
