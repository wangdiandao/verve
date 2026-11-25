//! Normalized host OS events and broadcast event bus.

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Default channel capacity for [`OsEventBus`].
pub const DEFAULT_EVENT_BUS_CAPACITY: usize = 64;

/// Normalized host power, network topology, and single-instance events emitted by platform listeners.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OsEvent {
    /// The host resumed from sleep or hibernation.
    WakeUp,
    /// Active network interfaces or routing tables changed.
    NetworkChanged,
    /// A secondary application launch requested activating the primary instance window.
    ActivatePrimaryInstance,
}

/// Broadcast bus for delivering [`OsEvent`] notifications to subscribers.
#[derive(Debug, Clone)]
pub struct OsEventBus {
    tx: broadcast::Sender<OsEvent>,
}

impl Default for OsEventBus {
    fn default() -> Self {
        Self::new(DEFAULT_EVENT_BUS_CAPACITY)
    }
}

impl OsEventBus {
    /// Creates a new event bus with the given ring buffer `capacity`.
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity.max(1));
        Self { tx }
    }

    /// Emits `event` to all active subscribers and returns the number of notified receivers.
    pub fn emit(&self, event: OsEvent) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    /// Subscribes to future [`OsEvent`] notifications emitted on this bus.
    pub fn subscribe(&self) -> broadcast::Receiver<OsEvent> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn event_bus_delivers_events_to_multiple_subscribers() {
        let bus = OsEventBus::default();
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        assert_eq!(bus.emit(OsEvent::WakeUp), 2);
        assert_eq!(bus.emit(OsEvent::NetworkChanged), 2);
        assert_eq!(bus.emit(OsEvent::ActivatePrimaryInstance), 2);

        for expected in [
            OsEvent::WakeUp,
            OsEvent::NetworkChanged,
            OsEvent::ActivatePrimaryInstance,
        ] {
            assert_eq!(rx1.recv().await.unwrap(), expected);
            assert_eq!(rx2.recv().await.unwrap(), expected);
        }
    }
}
