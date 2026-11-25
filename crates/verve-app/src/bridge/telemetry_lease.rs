//! RAII [`UiTelemetryLease`] wrapping [`verve_core::TelemetryLease`] for GPUI entities.

use verve_core::TelemetryHub;
use verve_core::TelemetryLease;
use verve_core::kernel::{ProxyKernel, TelemetryKind};

/// Object-safe telemetry lease factory decoupling GPUI entities from generic `ProxyKernel` types.
pub trait TelemetrySource: Send + Sync + 'static {
    /// Acquires an RAII [`TelemetryLease`] keeping the stream for `kind` active.
    fn acquire_lease(&self, kind: TelemetryKind) -> TelemetryLease;

    /// Returns the current active lease reference count for `kind`.
    fn lease_ref_count(&self, kind: TelemetryKind) -> usize;
}

impl<K: ProxyKernel> TelemetrySource for TelemetryHub<K> {
    fn acquire_lease(&self, kind: TelemetryKind) -> TelemetryLease {
        self.acquire(kind)
    }

    fn lease_ref_count(&self, kind: TelemetryKind) -> usize {
        self.ref_count(kind)
    }
}

/// RAII telemetry stream guard held directly inside mounted GPUI feature or shell entities.
///
/// Dropping the entity (when navigating away or closing the GPUI window) automatically releases
/// the underlying [`TelemetryLease`] and tears down idle background WebSocket streams.
#[derive(Debug)]
pub struct UiTelemetryLease {
    lease: TelemetryLease,
}

impl UiTelemetryLease {
    /// Acquires a new [`UiTelemetryLease`] for `kind` from `source`.
    pub fn acquire(source: &dyn TelemetrySource, kind: TelemetryKind) -> Self {
        Self {
            lease: source.acquire_lease(kind),
        }
    }

    /// Returns the [`TelemetryKind`] held open by this guard.
    pub fn kind(&self) -> TelemetryKind {
        self.lease.kind()
    }
}
