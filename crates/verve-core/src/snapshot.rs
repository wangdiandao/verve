//! Lock-free sliced [`AppSnapshot`] projection and [`SnapshotStore`] broadcaster.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use verve_compiler::CompileDiagnostic;
use verve_config::{LocalMachineConfig, ProfileEntry, ProfileUsage, SyncableConfig};
use verve_kernel::{
    ConnectionInfo, ControlEndpoint, LogEntry, ProxyGroup, ProxyMode, ProxyNode, ProxyProviderInfo,
    RuleEntry, RuleProviderInfo, TrafficSample,
};

/// Monotonically increasing per-domain slice revision counter.
pub type Revision = u64;

/// Default broadcast channel capacity for [`DomainDirtyEvent`] notifications.
const DIRTY_BROADCAST_CAPACITY: usize = 256;

/// Identifies one of the six independent state domains in [`AppSnapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DomainKind {
    /// Runtime lifecycle, routing mode, and active profile summary slice.
    Runtime,
    /// High-frequency throughput, cumulative traffic, and memory telemetry slice.
    Telemetry,
    /// Proxy groups, outbound nodes, and proxy providers slice.
    Proxies,
    /// Profile registry, subscription usages, and compile diagnostics slice.
    Profiles,
    /// Active connections, routing rules, rule providers, and logs slice.
    Activity,
    /// Persisted syncable and local machine configuration slice.
    Settings,
}

/// Notification emitted whenever a specific [`DomainKind`] slice advances its [`Revision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainDirtyEvent {
    /// Domain slice that changed.
    pub domain: DomainKind,
    /// New revision of the mutated domain slice.
    pub revision: Revision,
}

/// Runtime kernel status, routing mode, and active endpoint summary slice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSlice {
    /// Monotonic revision for this slice.
    pub revision: Revision,
    /// Active global routing mode.
    pub proxy_mode: ProxyMode,
    /// Whether the supervised proxy kernel process is currently running.
    pub kernel_running: bool,
    /// OS process ID of the running kernel, if active.
    pub kernel_pid: Option<u32>,
    /// Identifier of the currently active profile, if selected.
    pub active_profile_id: Option<String>,
    /// Display name of the currently active profile, if selected.
    pub active_profile_name: Option<String>,
    /// Combined HTTP/SOCKS5 inbound listening port (`mixed-port`).
    pub mixed_port: u16,
    /// Whether OS system proxy takeover is enabled.
    pub system_proxy_enabled: bool,
    /// Whether TUN virtual interface capture is enabled.
    pub tun_enabled: bool,
    /// Active control-plane IPC endpoint, if configured.
    pub control_endpoint: Option<ControlEndpoint>,
    /// Most recent runtime/lifecycle error message, if any.
    pub last_error: Option<String>,
}

impl Default for RuntimeSlice {
    fn default() -> Self {
        let defaults = SyncableConfig::default();
        Self {
            revision: 0,
            proxy_mode: ProxyMode::Rule,
            kernel_running: false,
            kernel_pid: None,
            active_profile_id: None,
            active_profile_name: None,
            mixed_port: defaults.mixed_port,
            system_proxy_enabled: defaults.system_proxy_enabled,
            tun_enabled: defaults.tun_enabled,
            control_endpoint: None,
            last_error: None,
        }
    }
}

/// High-frequency throughput, memory, and traffic history ring buffer slice.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TelemetrySlice {
    /// Monotonic revision for this slice.
    pub revision: Revision,
    /// Latest uplink throughput in bytes per second.
    pub upload_bps: u64,
    /// Latest downlink throughput in bytes per second.
    pub download_bps: u64,
    /// Cumulative uploaded bytes reported by the kernel.
    pub total_upload_bytes: u64,
    /// Cumulative downloaded bytes reported by the kernel.
    pub total_download_bytes: u64,
    /// Latest in-use kernel memory footprint in bytes.
    pub memory_bytes: u64,
    /// Bounded ring buffer of recent traffic samples ordered oldest-to-newest.
    pub traffic_history: VecDeque<TrafficSample>,
}

/// Proxy groups, outbound nodes, and external proxy providers slice.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProxiesSlice {
    /// Monotonic revision for this slice.
    pub revision: Revision,
    /// Ordered proxy groups exposed by the kernel.
    pub groups: Vec<ProxyGroup>,
    /// All known proxy nodes keyed by unique node name.
    pub nodes: BTreeMap<String, ProxyNode>,
    /// Registered external proxy providers.
    pub providers: Vec<ProxyProviderInfo>,
}

/// Profile registry, subscription usage metadata, and compile diagnostic slice.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProfilesSlice {
    /// Monotonic revision for this slice.
    pub revision: Revision,
    /// Registered local and remote profiles.
    pub profiles: Vec<ProfileEntry>,
    /// Persisted subscription traffic and expiration metadata keyed by profile ID.
    pub usages: BTreeMap<String, ProfileUsage>,
    /// Identifier of the currently active profile, if selected.
    pub active_profile_id: Option<String>,
    /// Whether global declarative Merge (`overrides/global.merge.yaml`) is enabled.
    pub global_merge_enabled: bool,
    /// Whether global JS script (`overrides/global.script.js`) is enabled.
    pub global_script_enabled: bool,
    /// Structured compile or preflight diagnostic from the most recent failed activation.
    pub last_compile_diagnostic: Option<CompileDiagnostic>,
}

/// Active connections, routing rules, rule providers, and bounded log buffer slice.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ActivitySlice {
    /// Monotonic revision for this slice.
    pub revision: Revision,
    /// Snapshot of currently tracked active connections.
    pub connections: Vec<ConnectionInfo>,
    /// Active routing rules loaded in the kernel.
    pub rules: Vec<RuleEntry>,
    /// Registered external rule providers.
    pub rule_providers: Vec<RuleProviderInfo>,
    /// Bounded ring buffer of recent kernel log entries ordered oldest-to-newest.
    pub logs: VecDeque<LogEntry>,
}

/// Persisted syncable and machine-local configuration domain slice.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SettingsSlice {
    /// Monotonic revision for this slice.
    pub revision: Revision,
    /// Cross-device syncable configuration (`settings.toml`).
    pub syncable: SyncableConfig,
    /// Machine-local configuration (`local.toml`).
    pub local: LocalMachineConfig,
}

/// Immutable root snapshot composed of six independently versioned `Arc` domain slices.
#[derive(Debug, Clone, Default)]
pub struct AppSnapshot {
    /// Runtime lifecycle, mode, and active profile summary.
    pub runtime: Arc<RuntimeSlice>,
    /// Live throughput, memory, and traffic history.
    pub telemetry: Arc<TelemetrySlice>,
    /// Proxy groups, nodes, and proxy providers.
    pub proxies: Arc<ProxiesSlice>,
    /// Profile registry, usages, and compile diagnostics.
    pub profiles: Arc<ProfilesSlice>,
    /// Active connections, rules, rule providers, and logs.
    pub activity: Arc<ActivitySlice>,
    /// Syncable and machine-local configuration.
    pub settings: Arc<SettingsSlice>,
}

/// Lock-free readable store publishing sliced [`AppSnapshot`] updates and [`DomainDirtyEvent`]s.
#[derive(Debug)]
pub struct SnapshotStore {
    inner: ArcSwap<AppSnapshot>,
    write_lock: Mutex<()>,
    dirty_tx: broadcast::Sender<DomainDirtyEvent>,
}

impl Default for SnapshotStore {
    fn default() -> Self {
        Self::new(AppSnapshot::default())
    }
}

impl SnapshotStore {
    /// Creates a new [`SnapshotStore`] seeded with `initial`.
    pub fn new(initial: AppSnapshot) -> Self {
        let (dirty_tx, _) = broadcast::channel(DIRTY_BROADCAST_CAPACITY);
        Self {
            inner: ArcSwap::from_pointee(initial),
            write_lock: Mutex::new(()),
            dirty_tx,
        }
    }

    /// Returns a lock-free strong reference to the current [`AppSnapshot`].
    pub fn snapshot(&self) -> Arc<AppSnapshot> {
        self.inner.load_full()
    }

    /// Subscribes to per-domain dirty notifications emitted after each slice publication.
    pub fn subscribe(&self) -> broadcast::Receiver<DomainDirtyEvent> {
        self.dirty_tx.subscribe()
    }

    /// Mutates only [`RuntimeSlice`], increments its revision, preserves the other five `Arc`
    /// slice pointers, and broadcasts [`DomainKind::Runtime`].
    pub fn publish_runtime(&self, mutate: impl FnOnce(&mut RuntimeSlice)) -> Revision {
        let _guard = self
            .write_lock
            .lock()
            .expect("snapshot write lock poisoned");
        let current = self.inner.load_full();
        let mut next_slice = (*current.runtime).clone();
        mutate(&mut next_slice);
        let revision = current.runtime.revision + 1;
        next_slice.revision = revision;

        let next = AppSnapshot {
            runtime: Arc::new(next_slice),
            telemetry: Arc::clone(&current.telemetry),
            proxies: Arc::clone(&current.proxies),
            profiles: Arc::clone(&current.profiles),
            activity: Arc::clone(&current.activity),
            settings: Arc::clone(&current.settings),
        };
        self.commit_and_notify(next, DomainKind::Runtime, revision)
    }

    /// Mutates only [`TelemetrySlice`], increments its revision, preserves the other five `Arc`
    /// slice pointers, and broadcasts [`DomainKind::Telemetry`].
    pub fn publish_telemetry(&self, mutate: impl FnOnce(&mut TelemetrySlice)) -> Revision {
        let _guard = self
            .write_lock
            .lock()
            .expect("snapshot write lock poisoned");
        let current = self.inner.load_full();
        let mut next_slice = (*current.telemetry).clone();
        mutate(&mut next_slice);
        let revision = current.telemetry.revision + 1;
        next_slice.revision = revision;

        let next = AppSnapshot {
            runtime: Arc::clone(&current.runtime),
            telemetry: Arc::new(next_slice),
            proxies: Arc::clone(&current.proxies),
            profiles: Arc::clone(&current.profiles),
            activity: Arc::clone(&current.activity),
            settings: Arc::clone(&current.settings),
        };
        self.commit_and_notify(next, DomainKind::Telemetry, revision)
    }

    /// Mutates only [`ProxiesSlice`], increments its revision, preserves the other five `Arc`
    /// slice pointers, and broadcasts [`DomainKind::Proxies`].
    pub fn publish_proxies(&self, mutate: impl FnOnce(&mut ProxiesSlice)) -> Revision {
        let _guard = self
            .write_lock
            .lock()
            .expect("snapshot write lock poisoned");
        let current = self.inner.load_full();
        let mut next_slice = (*current.proxies).clone();
        mutate(&mut next_slice);
        let revision = current.proxies.revision + 1;
        next_slice.revision = revision;

        let next = AppSnapshot {
            runtime: Arc::clone(&current.runtime),
            telemetry: Arc::clone(&current.telemetry),
            proxies: Arc::new(next_slice),
            profiles: Arc::clone(&current.profiles),
            activity: Arc::clone(&current.activity),
            settings: Arc::clone(&current.settings),
        };
        self.commit_and_notify(next, DomainKind::Proxies, revision)
    }

    /// Mutates only [`ProfilesSlice`], increments its revision, preserves the other five `Arc`
    /// slice pointers, and broadcasts [`DomainKind::Profiles`].
    pub fn publish_profiles(&self, mutate: impl FnOnce(&mut ProfilesSlice)) -> Revision {
        let _guard = self
            .write_lock
            .lock()
            .expect("snapshot write lock poisoned");
        let current = self.inner.load_full();
        let mut next_slice = (*current.profiles).clone();
        mutate(&mut next_slice);
        let revision = current.profiles.revision + 1;
        next_slice.revision = revision;

        let next = AppSnapshot {
            runtime: Arc::clone(&current.runtime),
            telemetry: Arc::clone(&current.telemetry),
            proxies: Arc::clone(&current.proxies),
            profiles: Arc::new(next_slice),
            activity: Arc::clone(&current.activity),
            settings: Arc::clone(&current.settings),
        };
        self.commit_and_notify(next, DomainKind::Profiles, revision)
    }

    /// Mutates only [`ActivitySlice`], increments its revision, preserves the other five `Arc`
    /// slice pointers, and broadcasts [`DomainKind::Activity`].
    pub fn publish_activity(&self, mutate: impl FnOnce(&mut ActivitySlice)) -> Revision {
        let _guard = self
            .write_lock
            .lock()
            .expect("snapshot write lock poisoned");
        let current = self.inner.load_full();
        let mut next_slice = (*current.activity).clone();
        mutate(&mut next_slice);
        let revision = current.activity.revision + 1;
        next_slice.revision = revision;

        let next = AppSnapshot {
            runtime: Arc::clone(&current.runtime),
            telemetry: Arc::clone(&current.telemetry),
            proxies: Arc::clone(&current.proxies),
            profiles: Arc::clone(&current.profiles),
            activity: Arc::new(next_slice),
            settings: Arc::clone(&current.settings),
        };
        self.commit_and_notify(next, DomainKind::Activity, revision)
    }

    /// Mutates only [`SettingsSlice`], increments its revision, preserves the other five `Arc`
    /// slice pointers, and broadcasts [`DomainKind::Settings`].
    pub fn publish_settings(&self, mutate: impl FnOnce(&mut SettingsSlice)) -> Revision {
        let _guard = self
            .write_lock
            .lock()
            .expect("snapshot write lock poisoned");
        let current = self.inner.load_full();
        let mut next_slice = (*current.settings).clone();
        mutate(&mut next_slice);
        let revision = current.settings.revision + 1;
        next_slice.revision = revision;

        let next = AppSnapshot {
            runtime: Arc::clone(&current.runtime),
            telemetry: Arc::clone(&current.telemetry),
            proxies: Arc::clone(&current.proxies),
            profiles: Arc::clone(&current.profiles),
            activity: Arc::clone(&current.activity),
            settings: Arc::new(next_slice),
        };
        self.commit_and_notify(next, DomainKind::Settings, revision)
    }

    fn commit_and_notify(
        &self,
        next: AppSnapshot,
        domain: DomainKind,
        revision: Revision,
    ) -> Revision {
        self.inner.store(Arc::new(next));
        let _ = self.dirty_tx.send(DomainDirtyEvent { domain, revision });
        revision
    }
}
