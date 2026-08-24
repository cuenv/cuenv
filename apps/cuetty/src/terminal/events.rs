use async_channel::{Receiver, Sender, TrySendError};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlEvent {
    ClipboardWrite(String),
    Title(String),
    Bell,
    Close,
}

/// Damage is coalesced; ordered control events are lossless.
pub struct EventBridge {
    wake_pending: AtomicBool,
    close_seen: AtomicBool,
    wake_tx: Sender<()>,
    controls: Mutex<VecDeque<ControlEvent>>,
}

impl EventBridge {
    pub fn new() -> (Arc<Self>, Receiver<()>) {
        let (wake_tx, wake_rx) = async_channel::bounded(1);
        (
            Arc::new(Self {
                wake_pending: AtomicBool::new(false),
                close_seen: AtomicBool::new(false),
                wake_tx,
                controls: Mutex::new(VecDeque::new()),
            }),
            wake_rx,
        )
    }

    pub fn wake(&self) {
        if !self.wake_pending.swap(true, Ordering::AcqRel)
            && let Err(TrySendError::Closed(())) = self.wake_tx.try_send(())
        {
            self.wake_pending.store(false, Ordering::Release);
        }
    }

    pub fn clipboard_write(&self, text: String) {
        self.control(ControlEvent::ClipboardWrite(text));
    }

    pub fn control(&self, event: ControlEvent) {
        self.controls
            .lock()
            .expect("event queue poisoned")
            .push_back(event);
        self.wake();
    }

    pub fn close(&self) {
        if !self.close_seen.swap(true, Ordering::AcqRel) {
            self.controls
                .lock()
                .expect("event queue poisoned")
                .push_back(ControlEvent::Close);
        }
        self.wake();
    }

    pub fn drain(&self) -> Vec<ControlEvent> {
        self.wake_pending.store(false, Ordering::Release);
        self.controls
            .lock()
            .expect("event queue poisoned")
            .drain(..)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wake_storm_is_coalesced() {
        let (bridge, rx) = EventBridge::new();
        for _ in 0..1000 {
            bridge.wake();
        }
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn control_events_keep_order_and_close_is_exactly_once() {
        let (bridge, _) = EventBridge::new();
        bridge.clipboard_write("one".into());
        bridge.control(ControlEvent::Title("shell".into()));
        bridge.control(ControlEvent::Bell);
        bridge.clipboard_write("two".into());
        bridge.close();
        bridge.close();
        assert_eq!(
            bridge.drain(),
            vec![
                ControlEvent::ClipboardWrite("one".into()),
                ControlEvent::Title("shell".into()),
                ControlEvent::Bell,
                ControlEvent::ClipboardWrite("two".into()),
                ControlEvent::Close
            ]
        );
    }
    #[test]
    fn wake_during_drain_gets_a_new_token() {
        let (bridge, rx) = EventBridge::new();
        bridge.wake();
        assert!(rx.try_recv().is_ok());
        bridge.wake();
        assert!(bridge.drain().is_empty());
        bridge.wake();
        assert!(rx.try_recv().is_ok());
    }
}
