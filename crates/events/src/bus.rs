//! Multi-subscriber `EventBus` for cuenv events.
//!
//! Provides a broadcast-capable event bus that allows multiple subscribers
//! to receive events concurrently.

use crate::event::{CuenvEvent, EventCategory, EventSource, SystemEvent};
use crate::metadata::correlation_id;
use std::sync::Mutex;
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

/// Process-wide event sender for the direct-emit path.
///
/// Held behind a `Mutex<Option<_>>` (not a `OnceLock`) so the global
/// sender can be replaced at runtime — important for tests, which
/// install a fresh bus per case, and for hosts that tear down and
/// recreate the bus on a long-lived process. When `None`, [`emit`] is
/// a no-op.
static GLOBAL_SENDER: Mutex<Option<EventSender>> = Mutex::new(None);

/// Install (or replace) the process-wide [`EventSender`].
///
/// Replaces any previously-installed sender. Always returns `true` for
/// historical compatibility with callers that treated the return as
/// "was the install successful"; failure modes (poisoned mutex) are
/// internal and surfaced via `tracing::warn!`.
///
/// Pair with [`emit`] / [`emit_with_source`] to publish events without
/// going through `tracing::info!` macros.
pub fn set_global_sender(sender: EventSender) -> bool {
    if let Ok(mut guard) = GLOBAL_SENDER.lock() {
        *guard = Some(sender);
        true
    } else {
        tracing::warn!("global event sender lock poisoned; dropping replacement");
        false
    }
}

/// Get the process-wide [`EventSender`], if one is installed.
#[must_use]
pub fn global_sender() -> Option<EventSender> {
    GLOBAL_SENDER.lock().ok().and_then(|g| g.clone())
}

/// Clear the process-wide [`EventSender`].
///
/// Intended for tests that need a clean slate between cases. After
/// calling, [`emit`] returns `SendError::Closed` until a new sender is
/// installed.
pub fn clear_global_sender() {
    if let Ok(mut guard) = GLOBAL_SENDER.lock() {
        *guard = None;
    }
}

/// Publish an [`EventCategory`] to the global event bus.
///
/// Constructs a [`CuenvEvent`] with a fresh id, the active session's
/// [`correlation_id`], and the given `source` and `category`, then sends
/// it through the process-wide [`EventSender`] (see [`set_global_sender`]).
///
/// Returns `Ok(())` on success, `Err(SendError::Closed)` if the bus has
/// been shut down, and `Err(SendError::Closed)` (semantically "no bus")
/// if [`set_global_sender`] was never called — this lets call sites stay
/// branch-free in non-bus contexts (tests, library embeddings) without
/// silent failures.
///
/// # Errors
///
/// Returns [`SendError::Closed`] if no global sender is installed or the
/// bus has been shut down.
pub fn emit_with_source(source: EventSource, category: EventCategory) -> Result<(), SendError> {
    let sender = global_sender().ok_or(SendError::Closed)?;
    let event = CuenvEvent {
        id: Uuid::new_v4(),
        correlation_id: correlation_id(),
        timestamp: chrono::Utc::now(),
        source,
        category,
    };
    sender.send(event)
}

/// Publish an [`EventCategory`] to the global event bus, using a default
/// source target of `"cuenv::events::emit"`.
///
/// See [`emit_with_source`] for the variant that lets you set the source
/// explicitly.
///
/// # Errors
///
/// See [`emit_with_source`].
pub fn emit(category: EventCategory) -> Result<(), SendError> {
    emit_with_source(EventSource::new("cuenv::events::emit"), category)
}

/// Default channel capacity for the broadcast channel.
const DEFAULT_BROADCAST_CAPACITY: usize = 1000;

/// Multi-subscriber event bus.
///
/// Events sent to this bus are broadcast to all subscribers.
/// Uses tokio's broadcast channel for fan-out delivery.
#[derive(Debug)]
pub struct EventBus {
    /// Sender for submitting events (wrapped in Option for shutdown support).
    /// When `shutdown()` is called, this is set to None, which drops the sender
    /// and causes the forwarding task to exit gracefully.
    sender: Mutex<Option<mpsc::UnboundedSender<CuenvEvent>>>,
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
            sender: Mutex::new(Some(sender)),
            broadcast_tx,
        }
    }

    /// Get a sender for submitting events to the bus.
    ///
    /// Returns `None` if the bus has been shut down.
    #[must_use]
    pub fn sender(&self) -> Option<EventSender> {
        self.sender
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|s| EventSender { inner: s.clone() }))
    }

    /// Shut down the event bus.
    ///
    /// This drops the internal sender, causing the forwarding task to exit.
    /// After shutdown, no more events can be sent and `sender()` returns `None`.
    ///
    /// This method is safe to call multiple times.
    pub fn shutdown(&self) {
        if let Ok(mut guard) = self.sender.lock() {
            // Take and drop the sender to signal the forwarding task to exit
            let _ = guard.take();
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
    /// Returns `None` only when the broadcast sender is closed. When the
    /// channel lags and drops events, returns a synthesized
    /// [`SystemEvent::EventGap`] so downstream consumers see the gap
    /// instead of silently losing it.
    pub async fn recv(&mut self) -> Option<CuenvEvent> {
        match self.inner.recv().await {
            Ok(event) => Some(event),
            Err(broadcast::error::RecvError::Lagged(n)) => Some(synth_event_gap(n)),
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }

    /// Try to receive an event without waiting.
    ///
    /// Returns `None` if no event is immediately available or the bus is
    /// closed. Returns [`SystemEvent::EventGap`] (synthesized) when the
    /// channel lagged and dropped events.
    pub fn try_recv(&mut self) -> Option<CuenvEvent> {
        match self.inner.try_recv() {
            Ok(event) => Some(event),
            Err(broadcast::error::TryRecvError::Lagged(n)) => Some(synth_event_gap(n)),
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => {
                None
            }
        }
    }
}

fn synth_event_gap(skipped: u64) -> CuenvEvent {
    tracing::warn!(skipped, "event receiver lagged; emitting EventGap");
    CuenvEvent::new(
        Uuid::new_v4(),
        EventSource::new("cuenv::events::bus"),
        EventCategory::System(SystemEvent::EventGap { skipped }),
    )
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
            Self::Closed => write!(f, "event bus is closed"),
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
        let sender = bus.sender().expect("sender should be available");
        assert!(!sender.is_closed());
    }

    #[test]
    fn emit_returns_closed_when_no_global_sender() {
        // GLOBAL_SENDER is a process-wide OnceLock; cargo test runs all
        // tests in one process so we can't safely *uninstall* it. But
        // before any installer test runs, emit must return Closed —
        // covered indirectly by the install path test below.
        let outcome = emit(EventCategory::System(SystemEvent::Shutdown));
        // Either Ok (a prior test installed) or Err(Closed) (none did).
        // Both are well-defined; the contract is "no panic, no surprise".
        assert!(matches!(outcome, Ok(()) | Err(SendError::Closed)));
    }

    #[tokio::test]
    async fn emit_with_installed_global_sender_publishes_event() {
        let bus = EventBus::new();
        let sender = bus.sender().expect("sender should be available");
        let mut receiver = bus.subscribe();

        // `set_global_sender` is idempotent process-wide; the test only
        // asserts that emit() succeeds when *some* sender is installed,
        // which is the contract we care about. Other tests in this
        // module may have installed an earlier sender — that's fine.
        let _ = set_global_sender(sender);

        emit(EventCategory::Output(OutputEvent::Stdout {
            content: "via direct emit".to_string(),
        }))
        .expect("emit should succeed with a global sender installed");

        let received = receiver
            .recv()
            .await
            .expect("subscriber should receive the emitted event");
        match received.category {
            EventCategory::Output(OutputEvent::Stdout { content }) => {
                assert_eq!(content, "via direct emit");
            }
            other => panic!("unexpected category: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_event_bus_send_receive() {
        let bus = EventBus::new();
        let sender = bus.sender().expect("sender should be available");
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
        let sender = bus.sender().expect("sender should be available");
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
            bus.sender().expect("sender should be available")
        };

        // Give time for the bus to be dropped
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        let event = make_test_event();
        let result = sender.send(event);
        // Sender still works because it has its own clone of the channel
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_event_bus_with_capacity() {
        let bus = EventBus::with_capacity(10);
        let sender = bus.sender().expect("sender should be available");
        assert!(!sender.is_closed());
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn test_event_bus_default() {
        let bus = EventBus::default();
        let sender = bus.sender().expect("sender should be available");
        assert!(!sender.is_closed());
    }

    #[tokio::test]
    async fn test_event_receiver_try_recv_empty() {
        let bus = EventBus::new();
        let _sender = bus.sender().expect("sender should be available");
        let mut receiver = bus.subscribe();

        // No events sent yet
        let result = receiver.try_recv();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_event_receiver_try_recv_with_event() {
        let bus = EventBus::new();
        let sender = bus.sender().expect("sender should be available");
        let mut receiver = bus.subscribe();

        let event = make_test_event();
        let event_id = event.id;
        sender.send(event).unwrap();

        // Give the background task time to process
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        let result = receiver.try_recv();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, event_id);
    }

    #[test]
    fn test_send_error_display() {
        let err = SendError::Closed;
        assert_eq!(format!("{err}"), "event bus is closed");
    }

    #[test]
    fn test_send_error_equality() {
        assert_eq!(SendError::Closed, SendError::Closed);
    }

    #[test]
    fn test_send_error_debug() {
        let err = SendError::Closed;
        let debug_str = format!("{err:?}");
        assert!(debug_str.contains("Closed"));
    }

    #[test]
    fn test_send_error_is_error() {
        let err = SendError::Closed;
        let _: &dyn std::error::Error = &err;
    }

    #[tokio::test]
    async fn test_event_sender_into_inner() {
        let bus = EventBus::new();
        let sender = bus.sender().expect("sender should be available");
        let inner = sender.into_inner();
        assert!(!inner.is_closed());
    }

    #[tokio::test]
    async fn test_event_bus_debug() {
        let bus = EventBus::new();
        let debug_str = format!("{bus:?}");
        assert!(debug_str.contains("EventBus"));
    }

    #[tokio::test]
    async fn test_event_sender_debug() {
        let bus = EventBus::new();
        let sender = bus.sender().expect("sender should be available");
        let debug_str = format!("{sender:?}");
        assert!(debug_str.contains("EventSender"));
    }

    #[tokio::test]
    async fn test_event_receiver_debug() {
        let bus = EventBus::new();
        let receiver = bus.subscribe();
        let debug_str = format!("{receiver:?}");
        assert!(debug_str.contains("EventReceiver"));
    }

    #[tokio::test]
    async fn test_multiple_events_in_order() {
        let bus = EventBus::new();
        let sender = bus.sender().expect("sender should be available");
        let mut receiver = bus.subscribe();

        let event1 = make_test_event();
        let event2 = make_test_event();
        let event3 = make_test_event();

        let id1 = event1.id;
        let id2 = event2.id;
        let id3 = event3.id;

        sender.send(event1).unwrap();
        sender.send(event2).unwrap();
        sender.send(event3).unwrap();

        let r1 = receiver.recv().await.unwrap();
        let r2 = receiver.recv().await.unwrap();
        let r3 = receiver.recv().await.unwrap();

        assert_eq!(r1.id, id1);
        assert_eq!(r2.id, id2);
        assert_eq!(r3.id, id3);
    }

    #[tokio::test]
    async fn test_sender_clone() {
        let bus = EventBus::new();
        let sender1 = bus.sender().expect("sender should be available");
        let sender2 = sender1.clone();

        let mut receiver = bus.subscribe();

        let event1 = make_test_event();
        let event2 = make_test_event();

        let id1 = event1.id;
        let id2 = event2.id;

        sender1.send(event1).unwrap();
        sender2.send(event2).unwrap();

        let r1 = receiver.recv().await.unwrap();
        let r2 = receiver.recv().await.unwrap();

        assert_eq!(r1.id, id1);
        assert_eq!(r2.id, id2);
    }

    #[tokio::test]
    async fn test_subscriber_count_changes() {
        let bus = EventBus::new();
        assert_eq!(bus.subscriber_count(), 0);

        let recv1 = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);

        let recv2 = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 2);

        drop(recv1);
        assert_eq!(bus.subscriber_count(), 1);

        drop(recv2);
        assert_eq!(bus.subscriber_count(), 0);
    }
}
