//! Desktop application composition root (`verve-app` library entrypoint).

#![warn(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

rust_i18n::i18n!("locales", fallback = "en-US");

pub mod bootstrap;
pub mod bridge;
pub mod features;
pub mod i18n;
pub mod shell;

pub use bridge::{CommandDispatcher, DomainChanged, TelemetrySource, UiStore, UiTelemetryLease};
pub use i18n::AppLocale;
pub use shell::{
    AppShell, ConfirmCallback, ConfirmDialogHost, ConfirmDialogSpec, NavigateCallback,
    QuitCallback, RouteDescriptor, RouteId, ShellHandle, SideSheetHost, SideSheetSpec,
    TrayController, WindowOpener, default_main_window_options, route_registry,
};
