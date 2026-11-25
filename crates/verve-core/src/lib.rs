//! Headless core coordinator, sliced snapshot publication, and telemetry hub for Verve.

mod commands;
mod coordinator;
mod error;
mod snapshot;
mod telemetry;

pub use commands::{AppCommand, CommandBus};
pub use coordinator::{
    AppCoordinator, CoordinatorConfig, CoordinatorHandle, MihomoCoordinatorBuilder,
};
pub use error::CoreError;
pub use snapshot::{
    ActivitySlice, AppSnapshot, DomainDirtyEvent, DomainKind, ProfilesSlice, ProxiesSlice,
    Revision, RuntimeSlice, SettingsSlice, SnapshotStore, TelemetrySlice,
};
pub use telemetry::{
    DEFAULT_LOG_CAPACITY, DEFAULT_TRAFFIC_HISTORY_CAPACITY, TelemetryHub, TelemetryLease,
};
pub use {
    verve_compiler as compiler, verve_config as config, verve_kernel as kernel,
    verve_kernel_mihomo as kernel_mihomo, verve_protocol as protocol, verve_sync as sync,
    verve_sys as sys,
};
