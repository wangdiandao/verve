//! Reference-counted on-demand [`TelemetryHub`] and RAII [`TelemetryLease`].

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use verve_kernel::{ProxyKernel, TelemetryEvent, TelemetryKind};

use crate::snapshot::SnapshotStore;

/// Default bounded capacity for [`crate::TelemetrySlice::traffic_history`].
pub const DEFAULT_TRAFFIC_HISTORY_CAPACITY: usize = 300;

/// Default bounded capacity for [`crate::ActivitySlice::logs`].
pub const DEFAULT_LOG_CAPACITY: usize = 1000;

const INITIAL_RETRY_BACKOFF: Duration = Duration::from_millis(250);
const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(2);
const SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_secs(2);

trait LeaseReleaser: Send + Sync {
    fn release(&self, kind: TelemetryKind);
}

/// RAII lease keeping a [`TelemetryKind`] stream active while held.
///
/// Dropping the last lease for a given [`TelemetryKind`] cancels the underlying
/// background stream task and closes its WebSocket connection.
pub struct TelemetryLease {
    kind: TelemetryKind,
    releaser: Arc<dyn LeaseReleaser>,
}

impl std::fmt::Debug for TelemetryLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelemetryLease")
            .field("kind", &self.kind)
            .finish()
    }
}

impl TelemetryLease {
    /// Returns the [`TelemetryKind`] kept alive by this lease.
    pub fn kind(&self) -> TelemetryKind {
        self.kind
    }
}

impl Drop for TelemetryLease {
    fn drop(&mut self) {
        self.releaser.release(self.kind);
    }
}

#[derive(Debug, Default)]
struct StreamSlot {
    ref_count: usize,
    generation: u64,
    cancel: Option<CancellationToken>,
    task: Option<JoinHandle<()>>,
}

struct TelemetryHubInner<K> {
    kernel: Arc<K>,
    snapshots: Arc<SnapshotStore>,
    traffic_capacity: usize,
    log_capacity: usize,
    root_token: CancellationToken,
    slots: Mutex<[StreamSlot; 4]>,
}

impl<K> Drop for TelemetryHubInner<K> {
    fn drop(&mut self) {
        self.root_token.cancel();
    }
}

impl<K: ProxyKernel> LeaseReleaser for TelemetryHubInner<K> {
    fn release(&self, kind: TelemetryKind) {
        let mut slots = self.slots.lock().expect("telemetry slots mutex poisoned");
        let slot = &mut slots[kind_index(kind)];
        slot.ref_count = slot.ref_count.saturating_sub(1);
        if slot.ref_count == 0
            && let Some(cancel) = slot.cancel.take()
        {
            cancel.cancel();
        }
    }
}

/// Reference-counted telemetry stream hub feeding bounded ring buffers in [`SnapshotStore`].
pub struct TelemetryHub<K: ProxyKernel> {
    inner: Arc<TelemetryHubInner<K>>,
}

impl<K: ProxyKernel> Clone for TelemetryHub<K> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K: ProxyKernel> std::fmt::Debug for TelemetryHub<K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelemetryHub")
            .field("traffic_capacity", &self.inner.traffic_capacity)
            .field("log_capacity", &self.inner.log_capacity)
            .finish()
    }
}

impl<K: ProxyKernel> TelemetryHub<K> {
    /// Creates a new [`TelemetryHub`] with default ring buffer capacities (`300` traffic, `1000` logs).
    pub fn new(kernel: Arc<K>, snapshots: Arc<SnapshotStore>) -> Self {
        Self::with_capacities(
            kernel,
            snapshots,
            DEFAULT_TRAFFIC_HISTORY_CAPACITY,
            DEFAULT_LOG_CAPACITY,
        )
    }

    /// Creates a new [`TelemetryHub`] with explicit ring buffer capacities.
    pub fn with_capacities(
        kernel: Arc<K>,
        snapshots: Arc<SnapshotStore>,
        traffic_capacity: usize,
        log_capacity: usize,
    ) -> Self {
        Self {
            inner: Arc::new(TelemetryHubInner {
                kernel,
                snapshots,
                traffic_capacity,
                log_capacity,
                root_token: CancellationToken::new(),
                slots: Mutex::new(Default::default()),
            }),
        }
    }

    /// Acquires an RAII [`TelemetryLease`] for `kind`.
    ///
    /// When the reference count transitions `0 -> 1`, a background task is spawned to open
    /// `kernel.open_telemetry(kind)` and publish updates into [`SnapshotStore`].
    pub fn acquire(&self, kind: TelemetryKind) -> TelemetryLease {
        let mut slots = self
            .inner
            .slots
            .lock()
            .expect("telemetry slots mutex poisoned");
        let slot = &mut slots[kind_index(kind)];
        slot.ref_count += 1;

        if slot.ref_count == 1 && !self.inner.root_token.is_cancelled() {
            slot.generation += 1;
            let generation = slot.generation;
            let cancel = self.inner.root_token.child_token();
            slot.cancel = Some(cancel.clone());

            let prev_task = slot.task.take();
            if let Some(prev) = prev_task {
                // Previous task's token is already cancelled; abort only if not finished.
                if !prev.is_finished() {
                    prev.abort();
                }
            }

            let inner = Arc::clone(&self.inner);
            slot.task = Some(tokio::spawn(async move {
                run_stream_loop(inner, kind, generation, cancel).await;
            }));
        }

        TelemetryLease {
            kind,
            releaser: Arc::clone(&self.inner) as Arc<dyn LeaseReleaser>,
        }
    }

    /// Returns the active lease reference count for `kind`.
    pub fn ref_count(&self, kind: TelemetryKind) -> usize {
        self.inner
            .slots
            .lock()
            .expect("telemetry slots mutex poisoned")[kind_index(kind)]
        .ref_count
    }

    /// Cancels all active telemetry stream tasks and awaits their termination.
    pub async fn shutdown(&self) {
        self.inner.root_token.cancel();

        let tasks: Vec<JoinHandle<()>> = {
            let mut slots = self
                .inner
                .slots
                .lock()
                .expect("telemetry slots mutex poisoned");
            slots
                .iter_mut()
                .filter_map(|slot| {
                    slot.ref_count = 0;
                    if let Some(cancel) = slot.cancel.take() {
                        cancel.cancel();
                    }
                    slot.task.take()
                })
                .collect()
        };

        for mut task in tasks {
            if tokio::time::timeout(SHUTDOWN_JOIN_TIMEOUT, &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
    }
}

async fn run_stream_loop<K: ProxyKernel>(
    inner: Arc<TelemetryHubInner<K>>,
    kind: TelemetryKind,
    generation: u64,
    cancel: CancellationToken,
) {
    let mut backoff = INITIAL_RETRY_BACKOFF;

    loop {
        if cancel.is_cancelled() {
            break;
        }

        let open_result = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            res = inner.kernel.open_telemetry(kind) => res,
        };

        let mut stream = match open_result {
            Ok(stream) => {
                backoff = INITIAL_RETRY_BACKOFF;
                stream
            }
            Err(err) => {
                tracing::debug!(
                    ?kind,
                    generation,
                    error = %err,
                    "failed to open telemetry stream; retrying"
                );
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(MAX_RETRY_BACKOFF);
                continue;
            }
        };

        let mut should_reconnect = false;
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                item = stream.next() => {
                    match item {
                        Some(Ok(event)) => apply_telemetry_event(&inner, event),
                        Some(Err(err)) => {
                            tracing::debug!(
                                ?kind,
                                generation,
                                error = %err,
                                "telemetry stream error; reconnecting"
                            );
                            should_reconnect = true;
                            break;
                        }
                        None => {
                            tracing::debug!(?kind, generation, "telemetry stream closed");
                            break;
                        }
                    }
                }
            }
        }

        if !should_reconnect || cancel.is_cancelled() {
            break;
        }

        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(MAX_RETRY_BACKOFF);
    }
}

fn apply_telemetry_event<K: ProxyKernel>(inner: &TelemetryHubInner<K>, event: TelemetryEvent) {
    match event {
        TelemetryEvent::Traffic(sample) => {
            inner.snapshots.publish_telemetry(|telemetry| {
                telemetry.upload_bps = sample.up_bps;
                telemetry.download_bps = sample.down_bps;
                push_bounded(
                    &mut telemetry.traffic_history,
                    sample,
                    inner.traffic_capacity,
                );
            });
        }
        TelemetryEvent::Memory(sample) => {
            inner.snapshots.publish_telemetry(|telemetry| {
                telemetry.memory_bytes = sample.inuse_bytes;
            });
        }
        TelemetryEvent::Connections {
            upload_total,
            download_total,
            connections,
        } => {
            inner.snapshots.publish_activity(|activity| {
                activity.connections = connections;
            });

            let current = inner.snapshots.snapshot();
            if current.telemetry.total_upload_bytes != upload_total
                || current.telemetry.total_download_bytes != download_total
            {
                inner.snapshots.publish_telemetry(|telemetry| {
                    telemetry.total_upload_bytes = upload_total;
                    telemetry.total_download_bytes = download_total;
                });
            }
        }
        TelemetryEvent::Log(entry) => {
            inner.snapshots.publish_activity(|activity| {
                push_bounded(&mut activity.logs, entry, inner.log_capacity);
            });
        }
    }
}

fn push_bounded<T>(ring: &mut VecDeque<T>, item: T, capacity: usize) {
    if capacity == 0 {
        return;
    }
    while ring.len() >= capacity {
        ring.pop_front();
    }
    ring.push_back(item);
}

const fn kind_index(kind: TelemetryKind) -> usize {
    match kind {
        TelemetryKind::Traffic => 0,
        TelemetryKind::Memory => 1,
        TelemetryKind::Connections => 2,
        TelemetryKind::Logs => 3,
    }
}
