use std::fmt;
use tokio::sync::mpsc;
use tracing::{Level, event};

/// Represents events that can occur within the cuenv system.
///
/// Events are used to communicate between different parts of the application,
/// such as user input, command execution progress, and system lifecycle events.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Event {
    /// User-initiated input event.
    UserInput {
        /// The raw input string from the user.
        input: String,
    },
    /// Indicates a command has started execution.
    CommandStart {
        /// The command being executed.
        command: String,
    },
    /// Reports progress during command execution.
    CommandProgress {
        /// The command being executed.
        command: String,
        /// Progress value between 0.0 and 1.0.
        progress: f32,
        /// Human-readable progress message.
        message: String,
    },
    /// Indicates a command has finished execution.
    CommandComplete {
        /// The command that was executed.
        command: String,
        /// Whether the command completed successfully.
        success: bool,
        /// The command output or error message.
        output: String,
    },
    /// System shutdown signal.
    SystemShutdown,
    /// Request to refresh the TUI display.
    TuiRefresh,
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UserInput { input } => write!(f, "UserInput: {input}"),
            Self::CommandStart { command } => write!(f, "CommandStart: {command}"),
            Self::CommandProgress {
                command,
                progress,
                message,
            } => {
                write!(
                    f,
                    "CommandProgress: {} ({:.1}%) - {}",
                    command,
                    progress * 100.0,
                    message
                )
            }
            Self::CommandComplete {
                command, success, ..
            } => {
                write!(
                    f,
                    "CommandComplete: {} ({})",
                    command,
                    if *success { "success" } else { "failed" }
                )
            }
            Self::SystemShutdown => write!(f, "SystemShutdown"),
            Self::TuiRefresh => write!(f, "TuiRefresh"),
        }
    }
}

/// Sender half of an unbounded event channel.
#[allow(dead_code)]
pub type EventSender = mpsc::UnboundedSender<Event>;

/// Receiver half of an unbounded event channel.
#[allow(dead_code)]
pub type EventReceiver = mpsc::UnboundedReceiver<Event>;

/// A message bus for broadcasting events between components.
///
/// The `EventBus` provides an unbounded channel for sending and receiving
/// events asynchronously. It can be split into separate sender and receiver
/// handles for use across different tasks.
#[allow(dead_code)]
pub struct EventBus {
    /// The sender half of the event channel.
    sender: EventSender,
    /// The receiver half of the event channel.
    receiver: EventReceiver,
}

#[allow(dead_code)]
impl EventBus {
    /// Creates a new event bus with an unbounded channel.
    #[must_use]
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self { sender, receiver }
    }

    /// Returns a clone of the sender handle.
    ///
    /// This allows multiple producers to send events to the same bus.
    #[must_use]
    pub fn sender(&self) -> EventSender {
        self.sender.clone()
    }

    /// Consumes the event bus and returns the sender and receiver handles.
    ///
    /// Use this when you need separate ownership of the sender and receiver,
    /// typically for distributing them across different tasks.
    #[must_use]
    pub fn split(self) -> (EventSender, EventReceiver) {
        (self.sender, self.receiver)
    }

    /// Sends an event through the bus, logging any errors.
    ///
    /// This method logs at DEBUG level when sending and ERROR level if
    /// the send fails (which can happen if all receivers are dropped).
    pub fn send_event(&self, event: Event) {
        event!(Level::DEBUG, "Sending event: {}", event);
        if let Err(e) = self.sender.send(event) {
            event!(Level::ERROR, "Failed to send event: {}", e);
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{Duration, timeout};

    #[tokio::test]
    async fn test_event_bus_creation() {
        let event_bus = EventBus::new();
        let sender = event_bus.sender();

        // Should be able to clone sender
        let _sender_clone = sender.clone();

        // Sender should not be closed
        assert!(!sender.is_closed());
    }

    #[tokio::test]
    async fn test_event_bus_split() {
        let event_bus = EventBus::new();
        let (sender, mut receiver) = event_bus.split();

        // Send an event
        let test_event = Event::UserInput {
            input: "test".to_string(),
        };
        sender.send(test_event.clone()).unwrap();

        // Receive the event
        let result = timeout(Duration::from_millis(100), receiver.recv()).await;
        assert!(result.is_ok());
        let event = result.unwrap().unwrap();

        match event {
            Event::UserInput { input } => assert_eq!(input, "test"),
            _ => panic!("Expected UserInput event"),
        }
    }

    #[tokio::test]
    async fn test_event_bus_send_event() {
        let event_bus = EventBus::new();
        let (sender, mut receiver) = event_bus.split();

        // Send event using sender directly
        let test_event = Event::CommandStart {
            command: "test_command".to_string(),
        };
        sender.send(test_event.clone()).unwrap();

        // Should receive the event
        let result = timeout(Duration::from_millis(100), receiver.recv()).await;
        assert!(result.is_ok());
        let event = result.unwrap().unwrap();

        match event {
            Event::CommandStart { command } => assert_eq!(command, "test_command"),
            _ => panic!("Expected CommandStart event"),
        }
    }

    #[tokio::test]
    async fn test_event_bus_send_event_method() {
        let mut event_bus = EventBus::new();
        // We need to get the receiver before using send_event
        let receiver = std::mem::replace(&mut event_bus.receiver, {
            let (_, new_receiver) = mpsc::unbounded_channel();
            new_receiver
        });
        let mut receiver = receiver;

        // Send event using send_event method
        let test_event = Event::CommandStart {
            command: "test_command".to_string(),
        };
        event_bus.send_event(test_event.clone());

        // Should receive the event
        let result = timeout(Duration::from_millis(100), receiver.recv()).await;
        assert!(result.is_ok());
        let event = result.unwrap().unwrap();

        match event {
            Event::CommandStart { command } => assert_eq!(command, "test_command"),
            _ => panic!("Expected CommandStart event"),
        }
    }

    #[tokio::test]
    async fn test_event_display_implementations() {
        let events = vec![
            Event::UserInput {
                input: "hello".to_string(),
            },
            Event::CommandStart {
                command: "build".to_string(),
            },
            Event::CommandProgress {
                command: "build".to_string(),
                progress: 0.5,
                message: "compiling".to_string(),
            },
            Event::CommandComplete {
                command: "build".to_string(),
                success: true,
                output: "done".to_string(),
            },
            Event::SystemShutdown,
            Event::TuiRefresh,
        ];

        for event in events {
            let display = format!("{event}");
            assert!(!display.is_empty());

            match event {
                Event::UserInput { .. } => assert!(display.contains("UserInput")),
                Event::CommandStart { .. } => assert!(display.contains("CommandStart")),
                Event::CommandProgress { .. } => {
                    assert!(display.contains("CommandProgress"));
                    assert!(display.contains("50.0%")); // progress should be formatted as percentage
                }
                Event::CommandComplete { .. } => {
                    assert!(display.contains("CommandComplete"));
                    assert!(display.contains("success"));
                }
                Event::SystemShutdown => assert_eq!(display, "SystemShutdown"),
                Event::TuiRefresh => assert_eq!(display, "TuiRefresh"),
            }
        }
    }

    #[tokio::test]
    async fn test_multiple_events() {
        let event_bus = EventBus::new();
        let (sender, mut receiver) = event_bus.split();

        // Send multiple events
        let events = vec![
            Event::CommandStart {
                command: "first".to_string(),
            },
            Event::CommandProgress {
                command: "first".to_string(),
                progress: 0.25,
                message: "starting".to_string(),
            },
            Event::CommandComplete {
                command: "first".to_string(),
                success: true,
                output: "completed".to_string(),
            },
        ];

        for event in events.clone() {
            sender.send(event).unwrap();
        }

        // Receive all events
        for expected_event in events {
            let result = timeout(Duration::from_millis(100), receiver.recv()).await;
            assert!(result.is_ok());
            let event = result.unwrap().unwrap();

            // Events should be received in order and match expected types
            match (&expected_event, &event) {
                (Event::CommandStart { .. }, Event::CommandStart { .. })
                | (Event::CommandProgress { .. }, Event::CommandProgress { .. })
                | (Event::CommandComplete { .. }, Event::CommandComplete { .. }) => {}
                _ => panic!("Event types don't match: expected {expected_event:?}, got {event:?}"),
            }
        }
    }

    #[test]
    fn test_event_bus_default() {
        let event_bus = EventBus::default();
        let sender = event_bus.sender();
        assert!(!sender.is_closed());
    }

    #[tokio::test]
    async fn test_event_clone() {
        let event = Event::UserInput {
            input: "test".to_string(),
        };
        let cloned_event = event.clone();

        match (event, cloned_event) {
            (Event::UserInput { input: input1 }, Event::UserInput { input: input2 }) => {
                assert_eq!(input1, input2);
            }
            _ => panic!("Clone didn't preserve event type"),
        }
    }

    #[tokio::test]
    async fn test_command_complete_failure() {
        let event = Event::CommandComplete {
            command: "failed_cmd".to_string(),
            success: false,
            output: "error output".to_string(),
        };

        let display = format!("{event}");
        assert!(display.contains("failed"));
        assert!(display.contains("failed_cmd"));
    }
}
