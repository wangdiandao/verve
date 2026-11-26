//! Headless core coordinator, sliced snapshot publication, and telemetry hub for Verve.

mod commands;
mod coordinator;
mod error;
mod service_ipc;
mod snapshot;
mod subscription;
mod telemetry;

pub use commands::{AppCommand, BatchDelayResults, CommandBus, ServiceControlAction};
pub use coordinator::{
    AppCoordinator, CoordinatorConfig, CoordinatorHandle, MihomoCoordinatorBuilder,
};
pub use error::CoreError;
pub use service_ipc::{
    OsServiceControlBackend, ProtocolServiceIpcClient, ServiceControlBackend,
    default_service_ipc_endpoint,
};
pub use snapshot::{
    ActivitySlice, AppSnapshot, DomainDirtyEvent, DomainKind, ProfilesSlice, ProxiesSlice,
    Revision, RuntimeSlice, ServiceStatus, SettingsSlice, SnapshotStore, TelemetrySlice,
};
pub use subscription::{
    FetchedSubscription, ReqwestSubscriptionFetcher, SubscriptionFetcher, SubscriptionUserInfo,
    generate_profile_id, infer_profile_name, parse_subscription_userinfo, validate_profile_yaml,
};
pub use telemetry::{
    DEFAULT_LOG_CAPACITY, DEFAULT_TRAFFIC_HISTORY_CAPACITY, TelemetryHub, TelemetryLease,
};
pub use {
    verve_compiler as compiler, verve_config as config, verve_kernel as kernel,
    verve_kernel_mihomo as kernel_mihomo, verve_protocol as protocol, verve_sync as sync,
    verve_sys as sys,
};
