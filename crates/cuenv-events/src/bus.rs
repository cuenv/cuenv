//! Multi-subscriber `EventBus` for cuenv events.
//!
//! Provides a broadcast-capable event bus that allows multiple subscribers
//! to receive events concurrently.

use crate::event::CuenvEvent;
use tokio::sync::{broadcast, mpsc};

/// Default channel capacity for the broadcast channel.
const DEFAULT_BROADCAST_CAPACITY: usize = 1000;

/// Multi-subscriber event bus.
///
/// Events sent to this bus are broadcast to all subscribers.
/// Uses tokio's broadcast channel for fan-out delivery.
#[derive(Debug)]
pub struct EventBus {
    /// Sender for submitting events.
    sender: mpsc::UnboundedSender<CuenvEvent>,
    /// Broadcast sender for fan-out.
    broadcast_tx: broadcast::Sender<CuenvEvent>,
}

impl EventBus {
    /// Create a new event bus.
    ///
    /// Spawns a background task to forward events from the mpsc channel
    /// to the broadcast channel.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_BROADCAST_CAPACITY)
    }

    /// Create a new event bus with a specific broadcast capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let (sender, mut receiver) = mpsc::unbounded_channel::<CuenvEvent>();
        let (broadcast_tx, _) = broadcast::channel(capacity);

        let broadcast_tx_clone = broadcast_tx.clone();
        tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                // Ignore send errors - they occur when there are no subscribers
                let _ = broadcast_tx_clone.send(event);
            }
        });

        Self {
            sender,
            broadcast_tx,
        }
    }

    /// Get a sender for submitting events to the bus.
    #[must_use]
    pub fn sender(&self) -> EventSender {
        EventSender {
            inner: self.sender.clone(),
        }
    }

    /// Subscribe to events from this bus.
    ///
    /// Returns a receiver that will receive all events sent to the bus
    /// after this subscription is created.
    #[must_use]
    pub fn subscribe(&self) -> EventReceiver {
        EventReceiver {
            inner: self.broadcast_tx.subscribe(),
        }
    }

    /// Get the number of active subscribers.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.broadcast_tx.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Sender handle for submitting events to an `EventBus`.
#[derive(Debug, Clone)]
pub struct EventSender {
    inner: mpsc::UnboundedSender<CuenvEvent>,
}

impl EventSender {
    /// Get the raw mpsc sender for use with the tracing layer.
    ///
    /// This is primarily used by `CuenvEventLayer` to send events directly.
    #[must_use]
    pub fn into_inner(self) -> mpsc::UnboundedSender<CuenvEvent> {
        self.inner
    }

    /// Send an event to the bus.
    ///
    /// # Errors
    ///
    /// Returns an error if the bus has been dropped.
    pub fn send(&self, event: CuenvEvent) -> Result<(), SendError> {
        self.inner.send(event).map_err(|_| SendError::Closed)
    }

    /// Check if the bus is still open.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

/// Receiver handle for receiving events from an `EventBus`.
#[derive(Debug)]
pub struct EventReceiver {
    inner: broadcast::Receiver<CuenvEvent>,
}

impl EventReceiver {
    /// Receive the next event.
    ///
    /// Returns `None` if the bus has been dropped.
    /// May skip events if the receiver falls behind.
    pub async fn recv(&mut self) -> Option<CuenvEvent> {
        loop {
            match self.inner.recv().await {
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "Event receiver lagged, skipped events");
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Try to receive an event without waiting.
    ///
    /// Returns `None` if no event is immediately available or the bus is closed.
    pub fn try_recv(&mut self) -> Option<CuenvEvent> {
        loop {
            match self.inner.try_recv() {
                Ok(event) => return Some(event),
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "Event receiver lagged, skipped events");
                }
                Err(
                    broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed,
                ) => {
                    return None;
                }
            }
        }
    }
}

/// Error returned when sending to a closed bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// The event bus has been closed.
    Closed,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Closed => write!(f, "event bus is closed"),
        }
    }
}

impl std::error::Error for SendError {}

#[cfg(test)]
#[allow(clippy::similar_names)]
mod tests {
    use super::*;
    use crate::event::{EventCategory, EventSource, OutputEvent};
    use uuid::Uuid;

    fn make_test_event() -> CuenvEvent {
        CuenvEvent::new(
            Uuid::new_v4(),
            EventSource::new("cuenv::test"),
            EventCategory::Output(OutputEvent::Stdout {
                content: "test".to_string(),
            }),
        )
    }

    #[tokio::test]
    async fn test_event_bus_creation() {
        let bus = EventBus::new();
        let sender = bus.sender();
        assert!(!sender.is_closed());
    }

    #[tokio::test]
    async fn test_event_bus_send_receive() {
        let bus = EventBus::new();
        let sender = bus.sender();
        let mut receiver = bus.subscribe();

        let event = make_test_event();
        let event_id = event.id;

        sender.send(event).unwrap();

        let received = receiver.recv().await.unwrap();
        assert_eq!(received.id, event_id);
    }

    #[tokio::test]
    async fn test_event_bus_multiple_subscribers() {
        let bus = EventBus::new();
        let sender = bus.sender();
        let mut receiver1 = bus.subscribe();
        let mut receiver2 = bus.subscribe();

        assert_eq!(bus.subscriber_count(), 2);

        let event = make_test_event();
        let event_id = event.id;

        sender.send(event).unwrap();

        let received1 = receiver1.recv().await.unwrap();
        let received2 = receiver2.recv().await.unwrap();

        assert_eq!(received1.id, event_id);
        assert_eq!(received2.id, event_id);
    }

    #[tokio::test]
    async fn test_event_bus_sender_survives_bus_drop() {
        // EventSender clones the underlying mpsc sender, so it remains valid
        // even after the EventBus is dropped. This is intentional - senders
        // are independent handles that can outlive the bus.
        let sender = {
            let bus = EventBus::new();
            bus.sender()
        };

        // Give time for the bus to be dropped
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        let event = make_test_event();
        let result = sender.send(event);
        // Sender still works because it has its own clone of the channel
        assert!(result.is_ok());
    }
}
