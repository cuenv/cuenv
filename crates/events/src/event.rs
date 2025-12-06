//! Event type definitions for structured cuenv events.
//!
//! This module defines the core event types that flow through the cuenv event system.
//! Events are categorized by domain (Task, CI, Command, etc.) and include rich metadata.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A structured cuenv event with full metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CuenvEvent {
    /// Unique event identifier.
    pub id: Uuid,
    /// Correlation ID for request tracing across operations.
    pub correlation_id: Uuid,
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// Source information for the event.
    pub source: EventSource,
    /// The event category and data.
    pub category: EventCategory,
}

impl CuenvEvent {
    /// Create a new event with the given category.
    #[must_use]
    pub fn new(correlation_id: Uuid, source: EventSource, category: EventCategory) -> Self {
        Self {
            id: Uuid::new_v4(),
            correlation_id,
            timestamp: Utc::now(),
            source,
            category,
        }
    }
}

/// Source information for an event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventSource {
    /// The tracing target (e.g., "`cuenv::task`", "`cuenv::ci`").
    pub target: String,
    /// Source file path, if available.
    pub file: Option<String>,
    /// Source line number, if available.
    pub line: Option<u32>,
}

impl EventSource {
    /// Create a new event source with just a target.
    #[must_use]
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            file: None,
            line: None,
        }
    }

    /// Create a new event source with file and line information.
    #[must_use]
    pub fn with_location(target: impl Into<String>, file: impl Into<String>, line: u32) -> Self {
        Self {
            target: target.into(),
            file: Some(file.into()),
            line: Some(line),
        }
    }
}

/// Event categories organized by domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum EventCategory {
    /// Task execution lifecycle events.
    Task(TaskEvent),
    /// CI pipeline events.
    Ci(CiEvent),
    /// Command lifecycle events.
    Command(CommandEvent),
    /// User interaction events.
    Interactive(InteractiveEvent),
    /// System/supervisor events.
    System(SystemEvent),
    /// Generic output events (for migration and compatibility).
    Output(OutputEvent),
}

/// Task execution lifecycle events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum TaskEvent {
    /// Task execution started.
    Started {
        /// Task name.
        name: String,
        /// Command being executed.
        command: String,
        /// Whether this is a hermetic execution.
        hermetic: bool,
    },
    /// Task cache hit - using cached result.
    CacheHit {
        /// Task name.
        name: String,
        /// Cache key that matched.
        cache_key: String,
    },
    /// Task cache miss - will execute.
    CacheMiss {
        /// Task name.
        name: String,
    },
    /// Task produced output.
    Output {
        /// Task name.
        name: String,
        /// Output stream.
        stream: Stream,
        /// Output content.
        content: String,
    },
    /// Task execution completed.
    Completed {
        /// Task name.
        name: String,
        /// Whether the task succeeded.
        success: bool,
        /// Exit code, if available.
        exit_code: Option<i32>,
        /// Duration in milliseconds.
        duration_ms: u64,
    },
    /// Task group execution started.
    GroupStarted {
        /// Group name/prefix.
        name: String,
        /// Whether tasks run sequentially.
        sequential: bool,
        /// Number of tasks in the group.
        task_count: usize,
    },
    /// Task group execution completed.
    GroupCompleted {
        /// Group name/prefix.
        name: String,
        /// Whether all tasks succeeded.
        success: bool,
        /// Duration in milliseconds.
        duration_ms: u64,
    },
}

/// CI pipeline events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum CiEvent {
    /// CI context detected.
    ContextDetected {
        /// CI provider name.
        provider: String,
        /// Event type (push, `pull_request`, etc.).
        event_type: String,
        /// Git ref name.
        ref_name: String,
    },
    /// Changed files found.
    ChangedFilesFound {
        /// Number of changed files.
        count: usize,
    },
    /// Projects discovered.
    ProjectsDiscovered {
        /// Number of projects found.
        count: usize,
    },
    /// Project skipped (no affected tasks).
    ProjectSkipped {
        /// Project path.
        path: String,
        /// Reason for skipping.
        reason: String,
    },
    /// Task executing within CI.
    TaskExecuting {
        /// Project path.
        project: String,
        /// Task name.
        task: String,
    },
    /// Task result within CI.
    TaskResult {
        /// Project path.
        project: String,
        /// Task name.
        task: String,
        /// Whether the task succeeded.
        success: bool,
        /// Error message, if failed.
        error: Option<String>,
    },
    /// CI report generated.
    ReportGenerated {
        /// Report file path.
        path: String,
    },
}

/// Command lifecycle events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum CommandEvent {
    /// Command started.
    Started {
        /// Command name.
        command: String,
        /// Command arguments.
        args: Vec<String>,
    },
    /// Command progress update.
    Progress {
        /// Command name.
        command: String,
        /// Progress percentage (0.0 to 1.0).
        progress: f32,
        /// Progress message.
        message: String,
    },
    /// Command completed.
    Completed {
        /// Command name.
        command: String,
        /// Whether the command succeeded.
        success: bool,
        /// Duration in milliseconds.
        duration_ms: u64,
    },
}

/// User interaction events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum InteractiveEvent {
    /// Prompt requested from user.
    PromptRequested {
        /// Unique prompt identifier.
        prompt_id: String,
        /// The prompt message.
        message: String,
        /// Available options.
        options: Vec<String>,
    },
    /// Prompt resolved with user response.
    PromptResolved {
        /// Prompt identifier.
        prompt_id: String,
        /// User's response.
        response: String,
    },
    /// Wait/progress indicator.
    WaitProgress {
        /// What we're waiting for.
        target: String,
        /// Elapsed seconds.
        elapsed_secs: u64,
    },
}

/// System/supervisor events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum SystemEvent {
    /// Supervisor log message.
    SupervisorLog {
        /// Log tag/category.
        tag: String,
        /// Log message.
        message: String,
    },
    /// System shutdown.
    Shutdown,
}

/// Generic output events for migration and compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum OutputEvent {
    /// Standard output.
    Stdout {
        /// Content to output.
        content: String,
    },
    /// Standard error.
    Stderr {
        /// Content to output.
        content: String,
    },
}

/// Output stream identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_creation() {
        let event = CuenvEvent::new(
            Uuid::new_v4(),
            EventSource::new("cuenv::test"),
            EventCategory::Output(OutputEvent::Stdout {
                content: "test".to_string(),
            }),
        );

        assert!(!event.id.is_nil());
        assert_eq!(event.source.target, "cuenv::test");
    }

    #[test]
    fn test_event_serialization() {
        let event = CuenvEvent::new(
            Uuid::new_v4(),
            EventSource::new("cuenv::task"),
            EventCategory::Task(TaskEvent::Started {
                name: "build".to_string(),
                command: "cargo build".to_string(),
                hermetic: true,
            }),
        );

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("cuenv::task"));
        assert!(json.contains("build"));

        let parsed: CuenvEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, event.id);
    }

    #[test]
    fn test_event_source_with_location() {
        let source = EventSource::with_location("cuenv::task", "src/main.rs", 42);
        assert_eq!(source.target, "cuenv::task");
        assert_eq!(source.file, Some("src/main.rs".to_string()));
        assert_eq!(source.line, Some(42));
    }
}
