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
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
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
    /// Broadcast bus lagged — `skipped` events were dropped between
    /// this consumer's last successful `recv` and now. Emitted by
    /// [`crate::bus::EventReceiver`] so downstream consumers
    /// (recorders, renderers) can surface a gap indicator instead of
    /// silently dropping the events.
    EventGap {
        /// Number of events skipped by the broadcast channel.
        skipped: u64,
    },
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
#[path = "event_tests.rs"]
mod tests;
