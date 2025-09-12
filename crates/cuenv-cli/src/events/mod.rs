use std::fmt;
use tokio::sync::mpsc;
use tracing::{event, Level};

// Re-export uuid for correlation IDs
pub use uuid::Uuid;

// Helper function to generate correlation IDs
pub fn generate_correlation_id() -> String {
    Uuid::new_v4().to_string()
}

// Event emission functions for structured output
pub fn emit_version_info(version: &str, correlation_id: &str) {
    event!(
        Level::INFO,
        event_type = "version_info",
        version = version,
        correlation_id = correlation_id,
        "Version information displayed"
    );
}

pub fn emit_env_load_output(content: &str, json_mode: bool) {
    event!(
        Level::INFO,
        event_type = "env_load_output", 
        content = content,
        json_mode = json_mode,
        "Environment load output"
    );
}

pub fn emit_env_status_output(content: &str, json_mode: bool) {
    event!(
        Level::INFO,
        event_type = "env_status_output",
        content = content, 
        json_mode = json_mode,
        "Environment status output"
    );
}

pub fn emit_shell_init_output(content: &str, json_mode: bool) {
    event!(
        Level::INFO,
        event_type = "shell_init_output",
        content = content,
        json_mode = json_mode, 
        "Shell init output"
    );
}

pub fn emit_allow_command_output(content: &str, json_mode: bool) {
    event!(
        Level::INFO,
        event_type = "allow_command_output",
        content = content,
        json_mode = json_mode,
        "Allow command output"
    );
}

pub fn emit_env_print_output(content: &str) {
    event!(
        Level::INFO,
        event_type = "env_print_output",
        content = content,
        "Environment print output"
    );
}

pub fn emit_task_execution_output(content: &str) {
    event!(
        Level::INFO,
        event_type = "task_execution_output", 
        content = content,
        "Task execution output"
    );
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Event {
    UserInput {
        input: String,
    },
    CommandStart {
        command: String,
    },
    CommandProgress {
        command: String,
        progress: f32,
        message: String,
    },
    CommandComplete {
        command: String,
        success: bool,
        output: String,
    },
    // New structured output events
    VersionInfo {
        version: String,
        correlation_id: String,
    },
    EnvLoadOutput {
        content: String,
        json_mode: bool,
    },
    EnvStatusOutput {
        content: String,
        json_mode: bool,
    },
    ShellInitOutput {
        content: String,
        json_mode: bool,
    },
    AllowCommandOutput {
        content: String,
        json_mode: bool,
    },
    EnvPrintOutput {
        content: String,
    },
    TaskExecutionOutput {
        content: String,
    },
    SystemShutdown,
    TuiRefresh,
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::UserInput { input } => write!(f, "UserInput: {input}"),
            Event::CommandStart { command } => write!(f, "CommandStart: {command}"),
            Event::CommandProgress {
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
            Event::CommandComplete {
                command, success, ..
            } => {
                write!(
                    f,
                    "CommandComplete: {} ({})",
                    command,
                    if *success { "success" } else { "failed" }
                )
            }
            Event::VersionInfo { version, .. } => write!(f, "VersionInfo: {version}"),
            Event::EnvLoadOutput { content, json_mode } => {
                write!(f, "EnvLoadOutput: (json: {json_mode}) {content}")
            }
            Event::EnvStatusOutput { content, json_mode } => {
                write!(f, "EnvStatusOutput: (json: {json_mode}) {content}")
            }
            Event::ShellInitOutput { content, json_mode } => {
                write!(f, "ShellInitOutput: (json: {json_mode}) {content}")
            }
            Event::AllowCommandOutput { content, json_mode } => {
                write!(f, "AllowCommandOutput: (json: {json_mode}) {content}")
            }
            Event::EnvPrintOutput { content } => write!(f, "EnvPrintOutput: {content}"),
            Event::TaskExecutionOutput { content } => write!(f, "TaskExecutionOutput: {content}"),
            Event::SystemShutdown => write!(f, "SystemShutdown"),
            Event::TuiRefresh => write!(f, "TuiRefresh"),
        }
    }
}

#[allow(dead_code)]
pub type EventSender = mpsc::UnboundedSender<Event>;
#[allow(dead_code)]
pub type EventReceiver = mpsc::UnboundedReceiver<Event>;

#[allow(dead_code)]
pub struct EventBus {
    sender: EventSender,
    receiver: EventReceiver,
}

#[allow(dead_code)]
impl EventBus {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self { sender, receiver }
    }

    pub fn sender(&self) -> EventSender {
        self.sender.clone()
    }

    pub fn split(self) -> (EventSender, EventReceiver) {
        (self.sender, self.receiver)
    }

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
    use tokio::time::{timeout, Duration};

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
            Event::VersionInfo {
                version: "1.0.0".to_string(),
                correlation_id: "test-id".to_string(),
            },
            Event::EnvLoadOutput {
                content: "env loaded".to_string(),
                json_mode: false,
            },
            Event::EnvPrintOutput {
                content: "KEY=value".to_string(),
            },
            Event::TaskExecutionOutput {
                content: "task completed".to_string(),
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
                Event::VersionInfo { .. } => assert!(display.contains("VersionInfo")),
                Event::EnvLoadOutput { .. } => assert!(display.contains("EnvLoadOutput")),
                Event::EnvPrintOutput { .. } => assert!(display.contains("EnvPrintOutput")),
                Event::TaskExecutionOutput { .. } => assert!(display.contains("TaskExecutionOutput")),
                Event::SystemShutdown => assert_eq!(display, "SystemShutdown"),
                Event::TuiRefresh => assert_eq!(display, "TuiRefresh"),
                _ => {} // Handle other new events
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

    #[test]
    fn test_event_clone() {
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

    #[test]
    fn test_generate_correlation_id() {
        let id1 = generate_correlation_id();
        let id2 = generate_correlation_id();
        
        // Should generate different IDs
        assert_ne!(id1, id2);
        
        // Should be valid UUID format (36 chars with hyphens)
        assert_eq!(id1.len(), 36);
        assert!(id1.contains('-'));
    }

    #[test]
    fn test_event_emission_functions() {
        // Test that event emission functions don't panic
        emit_version_info("test-version", "test-correlation-id");
        emit_env_load_output("test-content", false);
        emit_env_status_output("test-status", true);
        emit_shell_init_output("test-script", false);
        emit_allow_command_output("test-allow", true);
        emit_env_print_output("KEY=value");
        emit_task_execution_output("task done");
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
