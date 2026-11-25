//! Selective per-domain GPUI snapshot projection store ([`UiStore`]).

use std::sync::Arc;

use gpui::{AsyncApp, Context, EventEmitter, WeakEntity};
use tokio::sync::broadcast::error::RecvError;
use verve_core::kernel::TelemetryKind;
use verve_core::{
    ActivitySlice, DomainKind, ProfilesSlice, ProxiesSlice, Revision, RuntimeSlice, SettingsSlice,
    SnapshotStore, TelemetrySlice,
};

use super::telemetry_lease::{TelemetrySource, UiTelemetryLease};

/// Event emitted by [`UiStore`] only when a specific [`DomainKind`] advances its [`Revision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DomainChanged(pub DomainKind);

/// GPUI entity projecting immutable `Arc<*Slice>` state from [`SnapshotStore`].
///
/// Subscribes to [`SnapshotStore::subscribe`] on GPUI's foreground async executor and notifies
/// listeners selectively only for domains whose [`Revision`] strictly incremented.
pub struct UiStore {
    snapshots: Arc<SnapshotStore>,
    telemetry_source: Arc<dyn TelemetrySource>,
    tokio: tokio::runtime::Handle,
    runtime: Arc<RuntimeSlice>,
    telemetry: Arc<TelemetrySlice>,
    proxies: Arc<ProxiesSlice>,
    profiles: Arc<ProfilesSlice>,
    activity: Arc<ActivitySlice>,
    settings: Arc<SettingsSlice>,
    _watcher_task: gpui::Task<()>,
}

impl EventEmitter<DomainChanged> for UiStore {}

impl UiStore {
    /// Creates a new [`UiStore`] seeded from `snapshots` and starts its selective dirty listener.
    pub fn new(
        snapshots: Arc<SnapshotStore>,
        telemetry_source: Arc<dyn TelemetrySource>,
        tokio: tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial = snapshots.snapshot();
        let mut dirty_rx = snapshots.subscribe();

        let watcher = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            while let Ok(_) | Err(RecvError::Lagged(_)) = dirty_rx.recv().await {
                if this
                    .update(cx, |store, cx| store.sync_from_snapshot(cx))
                    .is_err()
                {
                    break;
                }
            }
        });

        Self {
            snapshots,
            telemetry_source,
            tokio,
            runtime: initial.runtime.clone(),
            telemetry: initial.telemetry.clone(),
            proxies: initial.proxies.clone(),
            profiles: initial.profiles.clone(),
            activity: initial.activity.clone(),
            settings: initial.settings.clone(),
            _watcher_task: watcher,
        }
    }

    /// Returns the latest [`RuntimeSlice`].
    pub fn runtime(&self) -> &Arc<RuntimeSlice> {
        &self.runtime
    }

    /// Returns the latest [`TelemetrySlice`].
    pub fn telemetry(&self) -> &Arc<TelemetrySlice> {
        &self.telemetry
    }

    /// Returns the latest [`ProxiesSlice`].
    pub fn proxies(&self) -> &Arc<ProxiesSlice> {
        &self.proxies
    }

    /// Returns the latest [`ProfilesSlice`].
    pub fn profiles(&self) -> &Arc<ProfilesSlice> {
        &self.profiles
    }

    /// Returns the latest [`ActivitySlice`].
    pub fn activity(&self) -> &Arc<ActivitySlice> {
        &self.activity
    }

    /// Returns the latest [`SettingsSlice`].
    pub fn settings(&self) -> &Arc<SettingsSlice> {
        &self.settings
    }

    /// Returns the cached [`Revision`] for `domain`.
    pub fn revision(&self, domain: DomainKind) -> Revision {
        match domain {
            DomainKind::Runtime => self.runtime.revision,
            DomainKind::Telemetry => self.telemetry.revision,
            DomainKind::Proxies => self.proxies.revision,
            DomainKind::Profiles => self.profiles.revision,
            DomainKind::Activity => self.activity.revision,
            DomainKind::Settings => self.settings.revision,
        }
    }

    /// Acquires an RAII [`UiTelemetryLease`] tied to the caller entity's lifetime.
    pub fn acquire_telemetry(&self, kind: TelemetryKind) -> UiTelemetryLease {
        let _guard = self.tokio.enter();
        UiTelemetryLease::acquire(self.telemetry_source.as_ref(), kind)
    }

    /// Returns the current active lease count for `kind`.
    pub fn telemetry_ref_count(&self, kind: TelemetryKind) -> usize {
        self.telemetry_source.lease_ref_count(kind)
    }

    /// Synchronizes cached slices against the underlying [`SnapshotStore`], emitting
    /// [`DomainChanged`] only for slices whose revision incremented.
    pub fn sync_from_snapshot(&mut self, cx: &mut Context<Self>) {
        let next = self.snapshots.snapshot();
        let mut changed = false;

        if next.runtime.revision > self.runtime.revision {
            self.runtime = Arc::clone(&next.runtime);
            cx.emit(DomainChanged(DomainKind::Runtime));
            changed = true;
        }
        if next.telemetry.revision > self.telemetry.revision {
            self.telemetry = Arc::clone(&next.telemetry);
            cx.emit(DomainChanged(DomainKind::Telemetry));
            changed = true;
        }
        if next.proxies.revision > self.proxies.revision {
            self.proxies = Arc::clone(&next.proxies);
            cx.emit(DomainChanged(DomainKind::Proxies));
            changed = true;
        }
        if next.profiles.revision > self.profiles.revision {
            self.profiles = Arc::clone(&next.profiles);
            cx.emit(DomainChanged(DomainKind::Profiles));
            changed = true;
        }
        if next.activity.revision > self.activity.revision {
            self.activity = Arc::clone(&next.activity);
            cx.emit(DomainChanged(DomainKind::Activity));
            changed = true;
        }
        if next.settings.revision > self.settings.revision {
            self.settings = Arc::clone(&next.settings);
            cx.emit(DomainChanged(DomainKind::Settings));
            changed = true;
        }

        if changed {
            cx.notify();
        }
    }
}
