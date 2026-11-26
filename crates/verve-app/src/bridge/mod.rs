//! In-process zero-serialization bridge between `verve-core` (Tokio) and GPUI views.

pub mod commands;
pub mod store;
pub mod telemetry_lease;

pub use commands::{CommandDispatcher, ToastNotification};
pub use store::{DomainChanged, UiStore};
pub use telemetry_lease::{TelemetrySource, UiTelemetryLease};
