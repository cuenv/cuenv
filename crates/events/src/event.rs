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
    /// Service lifecycle events.
    Service(ServiceEvent),
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
        /// Task name (fully qualified, e.g. `group.child`).
        name: String,
        /// Command being executed.
        command: String,
        /// Whether this is a hermetic execution.
        hermetic: bool,
        /// Parent group prefix, if this task runs inside a group.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_group: Option<String>,
        /// Kind of node — distinguishes leaf tasks from groups/sequences.
        #[serde(default)]
        task_kind: TaskKind,
    },
    /// Task cache hit - using cached result.
    CacheHit {
        /// Task name.
        name: String,
        /// Cache key that matched.
        cache_key: String,
        /// Parent group prefix, if applicable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_group: Option<String>,
    },
    /// Task cache miss - will execute.
    CacheMiss {
        /// Task name.
        name: String,
        /// Parent group prefix, if applicable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_group: Option<String>,
    },
    /// Task was not eligible for caching.
    CacheSkipped {
        /// Task name.
        name: String,
        /// Parent group prefix, if applicable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_group: Option<String>,
        /// Why caching was skipped for this task.
        reason: CacheSkipReason,
    },
    /// Task is queued — graph picked it but parallelism cap blocks immediate start.
    Queued {
        /// Task name.
        name: String,
        /// Parent group prefix, if applicable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_group: Option<String>,
        /// Position in the ready queue (zero-based).
        queue_position: usize,
    },
    /// Task is being skipped (e.g. dependency failed under continue-on-error).
    Skipped {
        /// Task name.
        name: String,
        /// Parent group prefix, if applicable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_group: Option<String>,
        /// Why the task is skipped.
        reason: SkipReason,
    },
    /// Task is being retried.
    Retrying {
        /// Task name.
        name: String,
        /// Parent group prefix, if applicable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_group: Option<String>,
        /// Attempt number (1-based).
        attempt: u32,
        /// Maximum attempts allowed.
        max_attempts: u32,
    },
    /// Task produced output.
    Output {
        /// Task name.
        name: String,
        /// Output stream.
        stream: Stream,
        /// Output content.
        content: String,
        /// Parent group prefix, if applicable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_group: Option<String>,
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
        /// Parent group prefix, if applicable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_group: Option<String>,
    },
    /// Task group execution started.
    GroupStarted {
        /// Group name/prefix.
        name: String,
        /// Whether tasks run sequentially.
        sequential: bool,
        /// Number of tasks in the group.
        task_count: usize,
        /// Parent group prefix when groups nest.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_group: Option<String>,
        /// Concurrency cap for the group, if set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_concurrency: Option<u32>,
    },
    /// Task group execution completed.
    GroupCompleted {
        /// Group name/prefix.
        name: String,
        /// Whether all tasks succeeded.
        success: bool,
        /// Duration in milliseconds.
        duration_ms: u64,
        /// Parent group prefix when groups nest.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_group: Option<String>,
        /// Number of children that succeeded.
        #[serde(default)]
        succeeded: usize,
        /// Number of children that failed.
        #[serde(default)]
        failed: usize,
        /// Number of children that were skipped.
        #[serde(default)]
        skipped: usize,
    },
}

/// Discriminator for the kind of node a task event refers to.
///
/// Leaf tasks default to [`TaskKind::Task`]. Group / sequence containers carry
/// their own group-level events but child task events keep `TaskKind::Task`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// A leaf task (default).
    #[default]
    Task,
    /// A parallel group of children.
    Group,
    /// A sequential list of children.
    Sequence,
}

/// Why a task was not eligible for the action cache.
///
/// Renderers surface this so users understand why a task ran instead of
/// being served from cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheSkipReason {
    /// Task declared no `inputs` to hash.
    EmptyInputs,
    /// Inputs included a non-path reference (project/task ref).
    NonPathRef,
    /// Inputs hashed to zero files after resolution.
    NoResolvedInputs,
    /// Task carries task-level env vars resolved at execution time.
    RuntimeEnv,
    /// Cache was explicitly disabled for this task.
    Disabled {
        /// Optional human-readable reason from the cache config.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Task cache mode is `never`.
    NeverMode,
    /// Inputs could not be mapped onto the cache hasher root.
    HasherRootMismatch,
    /// Input hashing itself failed.
    HashFailed,
}

impl std::fmt::Display for CacheSkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyInputs => write!(f, "empty inputs"),
            Self::NonPathRef => write!(f, "non-path input"),
            Self::NoResolvedInputs => write!(f, "no resolved inputs"),
            Self::RuntimeEnv => write!(f, "runtime env"),
            Self::Disabled { reason: Some(r) } => write!(f, "disabled: {r}"),
            Self::Disabled { reason: None } => write!(f, "disabled"),
            Self::NeverMode => write!(f, "cache mode never"),
            Self::HasherRootMismatch => write!(f, "hasher root mismatch"),
            Self::HashFailed => write!(f, "hashing failed"),
        }
    }
}

/// Why a task was skipped at execution time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkipReason {
    /// A required dependency failed (under continue-on-error mode).
    DependencyFailed {
        /// Name of the failing dependency.
        dep: String,
    },
    /// Task was explicitly disabled.
    ManuallyDisabled,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DependencyFailed { dep } => write!(f, "dependency failed: {dep}"),
            Self::ManuallyDisabled => write!(f, "manually disabled"),
        }
    }
}

/// Service lifecycle events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum ServiceEvent {
    /// Service is pending — deps not yet ready.
    Pending {
        /// Service name.
        name: String,
    },
    /// Service is starting — spawned, awaiting readiness probe.
    Starting {
        /// Service name.
        name: String,
        /// Command being executed.
        command: String,
    },
    /// Service produced output.
    Output {
        /// Service name.
        name: String,
        /// Output stream.
        stream: Stream,
        /// Output content (single line).
        line: String,
    },
    /// Service is ready — probe satisfied.
    Ready {
        /// Service name.
        name: String,
        /// Time in ms from `Starting` to `Ready`.
        after_ms: u64,
    },
    /// Service readiness timed out.
    ReadyTimeout {
        /// Service name.
        name: String,
        /// Time in ms before timeout.
        after_ms: u64,
    },
    /// Service is restarting.
    Restarting {
        /// Service name.
        name: String,
        /// Reason for the restart.
        reason: RestartReason,
        /// Restart attempt number.
        attempt: u32,
    },
    /// Service is stopping.
    Stopping {
        /// Service name.
        name: String,
    },
    /// Service has stopped.
    Stopped {
        /// Service name.
        name: String,
        /// Process exit code, if available.
        exit_code: Option<i32>,
    },
    /// Service has failed.
    Failed {
        /// Service name.
        name: String,
        /// Error description.
        error: String,
    },
    /// File watcher detected changes for a service.
    Watch {
        /// Service name.
        name: String,
        /// Paths that changed.
        changed: Vec<String>,
    },
}

/// Reason a service is being restarted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RestartReason {
    /// Service process crashed (non-zero exit).
    Crashed,
    /// File watcher triggered restart.
    WatchTriggered,
    /// Manual restart via `cuenv restart`.
    Manual,
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
                parent_group: None,
                task_kind: TaskKind::Task,
            }),
        );

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("cuenv::task"));
        assert!(json.contains("build"));

        let parsed: CuenvEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, event.id);
    }

    #[test]
    fn test_started_backcompat_serde_no_parent_group() {
        let json = r#"{"event":"Started","data":{"name":"build","command":"cargo build","hermetic":true}}"#;
        let parsed: TaskEvent = serde_json::from_str(json).unwrap();
        match parsed {
            TaskEvent::Started {
                parent_group,
                task_kind,
                ..
            } => {
                assert_eq!(parent_group, None);
                assert_eq!(task_kind, TaskKind::Task);
            }
            _ => panic!("expected Started"),
        }
    }

    #[test]
    fn test_event_source_with_location() {
        let source = EventSource::with_location("cuenv::task", "src/main.rs", 42);
        assert_eq!(source.target, "cuenv::task");
        assert_eq!(source.file, Some("src/main.rs".to_string()));
        assert_eq!(source.line, Some(42));
    }

    #[test]
    fn test_event_source_new() {
        let source = EventSource::new("cuenv::ci");
        assert_eq!(source.target, "cuenv::ci");
        assert!(source.file.is_none());
        assert!(source.line.is_none());
    }

    #[test]
    fn test_task_event_cache_hit() {
        let event = TaskEvent::CacheHit {
            name: "test".to_string(),
            cache_key: "abc123".to_string(),
            parent_group: Some("ci".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("CacheHit"));
        assert!(json.contains("abc123"));
        assert!(json.contains("\"parent_group\":\"ci\""));
    }

    #[test]
    fn test_task_event_cache_miss() {
        let event = TaskEvent::CacheMiss {
            name: "test".to_string(),
            parent_group: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("CacheMiss"));
        // parent_group: None should not be serialized
        assert!(!json.contains("parent_group"));
    }

    #[test]
    fn test_task_event_cache_skipped() {
        let event = TaskEvent::CacheSkipped {
            name: "fmt".to_string(),
            parent_group: None,
            reason: CacheSkipReason::EmptyInputs,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("CacheSkipped"));
        assert!(json.contains("empty_inputs"));

        let parsed: TaskEvent = serde_json::from_str(&json).unwrap();
        match parsed {
            TaskEvent::CacheSkipped { reason, .. } => {
                assert_eq!(reason, CacheSkipReason::EmptyInputs);
            }
            _ => panic!("expected CacheSkipped"),
        }
    }

    #[test]
    fn test_task_event_queued() {
        let event = TaskEvent::Queued {
            name: "build".to_string(),
            parent_group: Some("ci".to_string()),
            queue_position: 3,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Queued"));
        assert!(json.contains("\"queue_position\":3"));
    }

    #[test]
    fn test_task_event_skipped_dependency_failed() {
        let event = TaskEvent::Skipped {
            name: "deploy".to_string(),
            parent_group: None,
            reason: SkipReason::DependencyFailed {
                dep: "build".to_string(),
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Skipped"));
        let parsed: TaskEvent = serde_json::from_str(&json).unwrap();
        match parsed {
            TaskEvent::Skipped { reason, .. } => {
                assert_eq!(
                    reason,
                    SkipReason::DependencyFailed {
                        dep: "build".to_string()
                    }
                );
            }
            _ => panic!("expected Skipped"),
        }
    }

    #[test]
    fn test_task_event_retrying() {
        let event = TaskEvent::Retrying {
            name: "flaky".to_string(),
            parent_group: None,
            attempt: 2,
            max_attempts: 3,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Retrying"));
        assert!(json.contains("\"attempt\":2"));
        assert!(json.contains("\"max_attempts\":3"));
    }

    #[test]
    fn test_task_event_output() {
        let event = TaskEvent::Output {
            name: "build".to_string(),
            stream: Stream::Stdout,
            content: "compiling...".to_string(),
            parent_group: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Output"));
        assert!(json.contains("Stdout"));
    }

    #[test]
    fn test_task_event_completed() {
        let event = TaskEvent::Completed {
            name: "build".to_string(),
            success: true,
            exit_code: Some(0),
            duration_ms: 1500,
            parent_group: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Completed"));
        assert!(json.contains("1500"));
    }

    #[test]
    fn test_task_event_group_started() {
        let event = TaskEvent::GroupStarted {
            name: "tests".to_string(),
            sequential: false,
            task_count: 5,
            parent_group: None,
            max_concurrency: Some(4),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("GroupStarted"));
        assert!(json.contains('5'));
        assert!(json.contains("\"max_concurrency\":4"));
    }

    #[test]
    fn test_task_event_group_completed() {
        let event = TaskEvent::GroupCompleted {
            name: "tests".to_string(),
            success: true,
            duration_ms: 3000,
            parent_group: None,
            succeeded: 4,
            failed: 1,
            skipped: 0,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("GroupCompleted"));
        assert!(json.contains("\"succeeded\":4"));
        assert!(json.contains("\"failed\":1"));
    }

    #[test]
    fn test_cache_skip_reason_display() {
        assert_eq!(format!("{}", CacheSkipReason::EmptyInputs), "empty inputs");
        assert_eq!(
            format!(
                "{}",
                CacheSkipReason::Disabled {
                    reason: Some("hermetic".to_string())
                }
            ),
            "disabled: hermetic"
        );
    }

    #[test]
    fn test_task_kind_default() {
        assert_eq!(TaskKind::default(), TaskKind::Task);
    }

    #[test]
    fn test_ci_event_context_detected() {
        let event = CiEvent::ContextDetected {
            provider: "github".to_string(),
            event_type: "push".to_string(),
            ref_name: "main".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("ContextDetected"));
        assert!(json.contains("github"));
    }

    #[test]
    fn test_ci_event_changed_files_found() {
        let event = CiEvent::ChangedFilesFound { count: 10 };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("ChangedFilesFound"));
        assert!(json.contains("10"));
    }

    #[test]
    fn test_ci_event_projects_discovered() {
        let event = CiEvent::ProjectsDiscovered { count: 3 };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("ProjectsDiscovered"));
    }

    #[test]
    fn test_ci_event_project_skipped() {
        let event = CiEvent::ProjectSkipped {
            path: "/project".to_string(),
            reason: "no changes".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("ProjectSkipped"));
        assert!(json.contains("no changes"));
    }

    #[test]
    fn test_ci_event_task_executing() {
        let event = CiEvent::TaskExecuting {
            project: "/app".to_string(),
            task: "build".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("TaskExecuting"));
    }

    #[test]
    fn test_ci_event_task_result() {
        let event = CiEvent::TaskResult {
            project: "/app".to_string(),
            task: "build".to_string(),
            success: false,
            error: Some("build failed".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("TaskResult"));
        assert!(json.contains("build failed"));
    }

    #[test]
    fn test_ci_event_report_generated() {
        let event = CiEvent::ReportGenerated {
            path: "/reports/ci.json".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("ReportGenerated"));
    }

    #[test]
    fn test_command_event_started() {
        let event = CommandEvent::Started {
            command: "sync".to_string(),
            args: vec!["--force".to_string()],
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Started"));
        assert!(json.contains("--force"));
    }

    #[test]
    fn test_command_event_progress() {
        let event = CommandEvent::Progress {
            command: "sync".to_string(),
            progress: 0.5,
            message: "halfway there".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Progress"));
        assert!(json.contains("0.5"));
    }

    #[test]
    fn test_command_event_completed() {
        let event = CommandEvent::Completed {
            command: "sync".to_string(),
            success: true,
            duration_ms: 500,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Completed"));
    }

    #[test]
    fn test_interactive_event_prompt_requested() {
        let event = InteractiveEvent::PromptRequested {
            prompt_id: "p1".to_string(),
            message: "Choose an option".to_string(),
            options: vec!["a".to_string(), "b".to_string()],
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("PromptRequested"));
        assert!(json.contains("Choose an option"));
    }

    #[test]
    fn test_interactive_event_prompt_resolved() {
        let event = InteractiveEvent::PromptResolved {
            prompt_id: "p1".to_string(),
            response: "a".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("PromptResolved"));
    }

    #[test]
    fn test_interactive_event_wait_progress() {
        let event = InteractiveEvent::WaitProgress {
            target: "lock".to_string(),
            elapsed_secs: 30,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("WaitProgress"));
        assert!(json.contains("30"));
    }

    #[test]
    fn test_system_event_supervisor_log() {
        let event = SystemEvent::SupervisorLog {
            tag: "coordinator".to_string(),
            message: "started".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("SupervisorLog"));
    }

    #[test]
    fn test_system_event_shutdown() {
        let event = SystemEvent::Shutdown;
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Shutdown"));
    }

    #[test]
    fn test_output_event_stdout() {
        let event = OutputEvent::Stdout {
            content: "hello".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Stdout"));
        assert!(json.contains("hello"));
    }

    #[test]
    fn test_output_event_stderr() {
        let event = OutputEvent::Stderr {
            content: "error".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Stderr"));
    }

    #[test]
    fn test_stream_enum() {
        assert_eq!(Stream::Stdout, Stream::Stdout);
        assert_ne!(Stream::Stdout, Stream::Stderr);

        let stdout_json = serde_json::to_string(&Stream::Stdout).unwrap();
        let stderr_json = serde_json::to_string(&Stream::Stderr).unwrap();

        assert!(stdout_json.contains("Stdout"));
        assert!(stderr_json.contains("Stderr"));
    }

    #[test]
    fn test_event_category_all_variants() {
        let categories = vec![
            EventCategory::Task(TaskEvent::CacheMiss {
                name: "test".to_string(),
                parent_group: None,
            }),
            EventCategory::Service(ServiceEvent::Pending {
                name: "db".to_string(),
            }),
            EventCategory::Ci(CiEvent::ProjectsDiscovered { count: 1 }),
            EventCategory::Command(CommandEvent::Started {
                command: "sync".to_string(),
                args: vec![],
            }),
            EventCategory::Interactive(InteractiveEvent::WaitProgress {
                target: "lock".to_string(),
                elapsed_secs: 0,
            }),
            EventCategory::System(SystemEvent::Shutdown),
            EventCategory::Output(OutputEvent::Stdout {
                content: "out".to_string(),
            }),
        ];

        for cat in categories {
            let json = serde_json::to_string(&cat).unwrap();
            let parsed: EventCategory = serde_json::from_str(&json).unwrap();
            // Verify round-trip works
            let json2 = serde_json::to_string(&parsed).unwrap();
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn test_cuenv_event_clone() {
        let event = CuenvEvent::new(
            Uuid::new_v4(),
            EventSource::new("cuenv::test"),
            EventCategory::System(SystemEvent::Shutdown),
        );
        let cloned = event.clone();
        assert_eq!(event.id, cloned.id);
        assert_eq!(event.correlation_id, cloned.correlation_id);
    }

    #[test]
    fn test_cuenv_event_debug() {
        let event = CuenvEvent::new(
            Uuid::new_v4(),
            EventSource::new("cuenv::test"),
            EventCategory::System(SystemEvent::Shutdown),
        );
        let debug_str = format!("{event:?}");
        assert!(debug_str.contains("CuenvEvent"));
    }

    #[test]
    fn test_service_event_pending() {
        let event = ServiceEvent::Pending {
            name: "db".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Pending"));
        assert!(json.contains("db"));
    }

    #[test]
    fn test_service_event_starting() {
        let event = ServiceEvent::Starting {
            name: "db".to_string(),
            command: "postgres -D /data".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Starting"));
        assert!(json.contains("postgres"));
    }

    #[test]
    fn test_service_event_ready() {
        let event = ServiceEvent::Ready {
            name: "db".to_string(),
            after_ms: 1200,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Ready"));
        assert!(json.contains("1200"));
    }

    #[test]
    fn test_service_event_restarting() {
        let event = ServiceEvent::Restarting {
            name: "api".to_string(),
            reason: RestartReason::WatchTriggered,
            attempt: 2,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Restarting"));
        assert!(json.contains("WatchTriggered"));
    }

    #[test]
    fn test_service_event_stopped() {
        let event = ServiceEvent::Stopped {
            name: "web".to_string(),
            exit_code: Some(0),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Stopped"));
    }

    #[test]
    fn test_service_event_failed() {
        let event = ServiceEvent::Failed {
            name: "api".to_string(),
            error: "readiness timeout".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Failed"));
        assert!(json.contains("readiness timeout"));
    }

    #[test]
    fn test_service_event_watch() {
        let event = ServiceEvent::Watch {
            name: "api".to_string(),
            changed: vec!["src/main.rs".to_string()],
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Watch"));
        assert!(json.contains("src/main.rs"));
    }

    #[test]
    fn test_service_event_output() {
        let event = ServiceEvent::Output {
            name: "db".to_string(),
            stream: Stream::Stdout,
            line: "ready to accept connections".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Output"));
        assert!(json.contains("ready to accept connections"));
    }

    #[test]
    fn test_restart_reason_serialization() {
        let reasons = vec![
            RestartReason::Crashed,
            RestartReason::WatchTriggered,
            RestartReason::Manual,
        ];
        for reason in reasons {
            let json = serde_json::to_string(&reason).unwrap();
            let parsed: RestartReason = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&parsed).unwrap();
            assert_eq!(json, json2);
        }
    }
}
