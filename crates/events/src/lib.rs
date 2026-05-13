//! Structured event system for cuenv.
//!
//! This crate provides a unified event system that enables multiple UI frontends
//! (CLI, TUI, Web) to subscribe to a single event stream. Events are emitted using
//! tracing macros and captured by a custom tracing Layer.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │                           cuenv-events crate                            │
//! │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌─────────────┐ │
//! │  │ Event Schema │  │ EventBus     │  │ Tracing Layer│  │ Renderers   │ │
//! │  │ (typed)      │  │ (broadcast)  │  │ (capture)    │  │ (CLI/JSON)  │ │
//! │  └──────────────┘  └──────────────┘  └──────────────┘  └─────────────┘ │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use cuenv_events::{EventBus, CuenvEventLayer, emit_task_started};
//! use tracing_subscriber::layer::SubscriberExt;
//! use tracing_subscriber::util::SubscriberInitExt;
//!
//! // Create event bus and layer
//! let bus = EventBus::new();
//! let layer = CuenvEventLayer::new(bus.sender().inner);
//!
//! // Initialize tracing with the layer
//! tracing_subscriber::registry()
//!     .with(layer)
//!     .init();
//!
//! // Emit events using macros
//! emit_task_started!("build", "cargo build", false);
//! ```

pub mod bus;
pub mod event;
pub mod layer;
pub mod metadata;
pub mod redaction;
pub mod renderers;
pub mod subscribers;

// Re-exports for convenience
pub use bus::{
    EventBus, EventReceiver, EventSender, SendError, emit, emit_with_source, global_sender,
    set_global_sender,
};
pub use event::{
    CacheSkipReason, CiEvent, CommandEvent, CuenvEvent, EventCategory, EventSource,
    InteractiveEvent, OutputEvent, RestartReason, ServiceEvent, SkipReason, Stream, SystemEvent,
    TaskEvent, TaskKind,
};
pub use layer::CuenvEventLayer;
pub use metadata::{MetadataContext, correlation_id, set_correlation_id};
pub use redaction::{REDACTED_PLACEHOLDER, redact, register_secret, register_secrets};
#[cfg(feature = "spinner")]
pub use renderers::SpinnerRenderer;
pub use renderers::{CliRenderer, JsonRenderer};
pub use subscribers::{EventRecorder, EventReplayReader, RecorderError};

// ============================================================================
// Emit Macros
// ============================================================================

/// Emit a task started event.
///
/// Two forms are supported:
/// - `emit_task_started!(name, command, hermetic)` — leaf task, no group.
/// - `emit_task_started!(name, command, hermetic, parent_group, task_kind)`
///   — leaf or group task, optionally inside a parent group.
///   `parent_group` is `Option<&str>` (or anything `Display`); `task_kind`
///   is a `&'static str` matching `TaskKind` (`"task"`, `"group"`, `"sequence"`).
#[macro_export]
macro_rules! emit_task_started {
    ($name:expr, $command:expr, $hermetic:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.started",
            task_name = %$name,
            command = %$command,
            hermetic = $hermetic,
        )
    };
    ($name:expr, $command:expr, $hermetic:expr, $parent_group:expr, $task_kind:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.started",
            task_name = %$name,
            command = %$command,
            hermetic = $hermetic,
            parent_group = ?$parent_group,
            task_kind = $task_kind,
        )
    };
}

/// Emit a task cache hit event.
///
/// Forms:
/// - `emit_task_cache_hit!(name, cache_key)`
/// - `emit_task_cache_hit!(name, cache_key, parent_group)`
#[macro_export]
macro_rules! emit_task_cache_hit {
    ($name:expr, $cache_key:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.cache_hit",
            task_name = %$name,
            cache_key = %$cache_key,
        )
    };
    ($name:expr, $cache_key:expr, $parent_group:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.cache_hit",
            task_name = %$name,
            cache_key = %$cache_key,
            parent_group = ?$parent_group,
        )
    };
}

/// Emit a task cache miss event.
///
/// Forms:
/// - `emit_task_cache_miss!(name)`
/// - `emit_task_cache_miss!(name, parent_group)`
#[macro_export]
macro_rules! emit_task_cache_miss {
    ($name:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.cache_miss",
            task_name = %$name,
        )
    };
    ($name:expr, $parent_group:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.cache_miss",
            task_name = %$name,
            parent_group = ?$parent_group,
        )
    };
}

/// Emit a task cache skipped event with a structured reason.
///
/// `reason` should be a [`crate::CacheSkipReason`].
///
/// Forms:
/// - `emit_task_cache_skipped!(name, reason)`
/// - `emit_task_cache_skipped!(name, reason, parent_group)`
#[macro_export]
macro_rules! emit_task_cache_skipped {
    ($name:expr, $reason:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.cache_skipped",
            task_name = %$name,
            cache_skip_reason = %$crate::__macro_helpers::encode_cache_skip_reason(&$reason),
        )
    };
    ($name:expr, $reason:expr, $parent_group:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.cache_skipped",
            task_name = %$name,
            cache_skip_reason = %$crate::__macro_helpers::encode_cache_skip_reason(&$reason),
            parent_group = ?$parent_group,
        )
    };
}

/// Emit a task queued event (graph picked task but parallelism cap blocks start).
///
/// Forms:
/// - `emit_task_queued!(name, queue_position)`
/// - `emit_task_queued!(name, queue_position, parent_group)`
#[macro_export]
macro_rules! emit_task_queued {
    ($name:expr, $queue_position:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.queued",
            task_name = %$name,
            queue_position = $queue_position,
        )
    };
    ($name:expr, $queue_position:expr, $parent_group:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.queued",
            task_name = %$name,
            queue_position = $queue_position,
            parent_group = ?$parent_group,
        )
    };
}

/// Emit a task skipped event with a structured reason.
///
/// `reason` should be a [`crate::SkipReason`].
///
/// Forms:
/// - `emit_task_skipped!(name, reason)`
/// - `emit_task_skipped!(name, reason, parent_group)`
#[macro_export]
macro_rules! emit_task_skipped {
    ($name:expr, $reason:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.skipped",
            task_name = %$name,
            skip_reason = %$crate::__macro_helpers::encode_skip_reason(&$reason),
        )
    };
    ($name:expr, $reason:expr, $parent_group:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.skipped",
            task_name = %$name,
            skip_reason = %$crate::__macro_helpers::encode_skip_reason(&$reason),
            parent_group = ?$parent_group,
        )
    };
}

/// Emit a task retrying event.
///
/// Forms:
/// - `emit_task_retrying!(name, attempt, max_attempts)`
/// - `emit_task_retrying!(name, attempt, max_attempts, parent_group)`
#[macro_export]
macro_rules! emit_task_retrying {
    ($name:expr, $attempt:expr, $max_attempts:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.retrying",
            task_name = %$name,
            attempt = $attempt,
            max_attempts = $max_attempts,
        )
    };
    ($name:expr, $attempt:expr, $max_attempts:expr, $parent_group:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.retrying",
            task_name = %$name,
            attempt = $attempt,
            max_attempts = $max_attempts,
            parent_group = ?$parent_group,
        )
    };
}

/// Emit a task output event.
///
/// Forms:
/// - `emit_task_output!(name, stream, content)`
/// - `emit_task_output!(name, stream, content, parent_group)`
#[macro_export]
macro_rules! emit_task_output {
    ($name:expr, $stream:expr, $content:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.output",
            task_name = %$name,
            stream = $stream,
            content = %$content,
        )
    };
    ($name:expr, $stream:expr, $content:expr, $parent_group:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.output",
            task_name = %$name,
            stream = $stream,
            content = %$content,
            parent_group = ?$parent_group,
        )
    };
}

/// Emit a task completed event.
///
/// Forms:
/// - `emit_task_completed!(name, success, exit_code, duration_ms)`
/// - `emit_task_completed!(name, success, exit_code, duration_ms, parent_group)`
#[macro_export]
macro_rules! emit_task_completed {
    ($name:expr, $success:expr, $exit_code:expr, $duration_ms:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.completed",
            task_name = %$name,
            success = $success,
            exit_code = ?$exit_code,
            duration_ms = $duration_ms,
        )
    };
    ($name:expr, $success:expr, $exit_code:expr, $duration_ms:expr, $parent_group:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.completed",
            task_name = %$name,
            success = $success,
            exit_code = ?$exit_code,
            duration_ms = $duration_ms,
            parent_group = ?$parent_group,
        )
    };
}

/// Emit a task group started event.
///
/// Forms:
/// - `emit_task_group_started!(name, sequential, task_count)`
/// - `emit_task_group_started!(name, sequential, task_count, parent_group, max_concurrency)`
#[macro_export]
macro_rules! emit_task_group_started {
    ($name:expr, $sequential:expr, $task_count:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.group_started",
            task_name = %$name,
            sequential = $sequential,
            task_count = $task_count,
        )
    };
    ($name:expr, $sequential:expr, $task_count:expr, $parent_group:expr, $max_concurrency:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.group_started",
            task_name = %$name,
            sequential = $sequential,
            task_count = $task_count,
            parent_group = ?$parent_group,
            max_concurrency = ?$max_concurrency,
        )
    };
}

/// Emit a task group completed event.
///
/// Forms:
/// - `emit_task_group_completed!(name, success, duration_ms)`
/// - `emit_task_group_completed!(name, success, duration_ms, succeeded, failed, skipped)`
/// - `emit_task_group_completed!(name, success, duration_ms, succeeded, failed, skipped, parent_group)`
#[macro_export]
macro_rules! emit_task_group_completed {
    ($name:expr, $success:expr, $duration_ms:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.group_completed",
            task_name = %$name,
            success = $success,
            duration_ms = $duration_ms,
        )
    };
    ($name:expr, $success:expr, $duration_ms:expr, $succeeded:expr, $failed:expr, $skipped:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.group_completed",
            task_name = %$name,
            success = $success,
            duration_ms = $duration_ms,
            succeeded = $succeeded,
            failed_count = $failed,
            skipped_count = $skipped,
        )
    };
    ($name:expr, $success:expr, $duration_ms:expr, $succeeded:expr, $failed:expr, $skipped:expr, $parent_group:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.group_completed",
            task_name = %$name,
            success = $success,
            duration_ms = $duration_ms,
            succeeded = $succeeded,
            failed_count = $failed,
            skipped_count = $skipped,
            parent_group = ?$parent_group,
        )
    };
}

// Service Events

/// Emit a service pending event.
///
/// # Example
/// ```rust,ignore
/// emit_service_pending!("db");
/// ```
#[macro_export]
macro_rules! emit_service_pending {
    ($name:expr) => {
        ::tracing::info!(
            target: "cuenv::service",
            event_type = "service.pending",
            service_name = %$name,
        )
    };
}

/// Emit a service starting event.
///
/// # Example
/// ```rust,ignore
/// emit_service_starting!("db", "postgres -D /data");
/// ```
#[macro_export]
macro_rules! emit_service_starting {
    ($name:expr, $command:expr) => {
        ::tracing::info!(
            target: "cuenv::service",
            event_type = "service.starting",
            service_name = %$name,
            command = %$command,
        )
    };
}

/// Emit a service output event.
///
/// # Example
/// ```rust,ignore
/// emit_service_output!("db", "stdout", "ready to accept connections");
/// ```
#[macro_export]
macro_rules! emit_service_output {
    ($name:expr, $stream:expr, $line:expr) => {
        ::tracing::info!(
            target: "cuenv::service",
            event_type = "service.output",
            service_name = %$name,
            stream = $stream,
            content = %$line,
        )
    };
}

/// Emit a service ready event.
///
/// # Example
/// ```rust,ignore
/// emit_service_ready!("db", 1200_u64);
/// ```
#[macro_export]
macro_rules! emit_service_ready {
    ($name:expr, $after_ms:expr) => {
        ::tracing::info!(
            target: "cuenv::service",
            event_type = "service.ready",
            service_name = %$name,
            after_ms = $after_ms,
        )
    };
}

/// Emit a service stopping event.
///
/// # Example
/// ```rust,ignore
/// emit_service_stopping!("db");
/// ```
#[macro_export]
macro_rules! emit_service_stopping {
    ($name:expr) => {
        ::tracing::info!(
            target: "cuenv::service",
            event_type = "service.stopping",
            service_name = %$name,
        )
    };
}

/// Emit a service stopped event.
///
/// # Example
/// ```rust,ignore
/// emit_service_stopped!("db", Some(0));
/// ```
#[macro_export]
macro_rules! emit_service_stopped {
    ($name:expr, $exit_code:expr) => {
        ::tracing::info!(
            target: "cuenv::service",
            event_type = "service.stopped",
            service_name = %$name,
            exit_code = ?$exit_code,
        )
    };
}

/// Emit a service failed event.
///
/// # Example
/// ```rust,ignore
/// emit_service_failed!("db", "readiness timeout");
/// ```
#[macro_export]
macro_rules! emit_service_failed {
    ($name:expr, $error:expr) => {
        ::tracing::info!(
            target: "cuenv::service",
            event_type = "service.failed",
            service_name = %$name,
            error = %$error,
        )
    };
}

/// Emit a service readiness timeout event.
///
/// # Example
/// ```rust,ignore
/// emit_service_ready_timeout!("db", 60000_u64);
/// ```
#[macro_export]
macro_rules! emit_service_ready_timeout {
    ($name:expr, $after_ms:expr) => {
        ::tracing::info!(
            target: "cuenv::service",
            event_type = "service.ready_timeout",
            service_name = %$name,
            after_ms = $after_ms,
        )
    };
}

/// Emit a service restarting event.
///
/// # Example
/// ```rust,ignore
/// emit_service_restarting!("api", "crashed", 2_u32);
/// ```
#[macro_export]
macro_rules! emit_service_restarting {
    ($name:expr, $reason:expr, $attempt:expr) => {
        ::tracing::info!(
            target: "cuenv::service",
            event_type = "service.restarting",
            service_name = %$name,
            reason = %$reason,
            attempt = $attempt,
        )
    };
}

/// Emit a service file watch event.
///
/// # Example
/// ```rust,ignore
/// emit_service_watch!("api", &["src/main.rs".to_string()]);
/// ```
#[macro_export]
macro_rules! emit_service_watch {
    ($name:expr, $changed:expr) => {
        ::tracing::info!(
            target: "cuenv::service",
            event_type = "service.watch",
            service_name = %$name,
            changed = ?$changed,
        )
    };
}

// CI Events

/// Emit a CI context detected event.
#[macro_export]
macro_rules! emit_ci_context {
    ($provider:expr, $event_type:expr, $ref_name:expr) => {
        ::tracing::info!(
            target: "cuenv::ci",
            event_type = "ci.context_detected",
            provider = %$provider,
            ci_event_type = %$event_type,
            ref_name = %$ref_name,
        )
    };
}

/// Emit a CI changed files found event.
#[macro_export]
macro_rules! emit_ci_changed_files {
    ($count:expr) => {
        ::tracing::info!(
            target: "cuenv::ci",
            event_type = "ci.changed_files",
            count = $count,
        )
    };
}

/// Emit a CI projects discovered event.
#[macro_export]
macro_rules! emit_ci_projects_discovered {
    ($count:expr) => {
        ::tracing::info!(
            target: "cuenv::ci",
            event_type = "ci.projects_discovered",
            count = $count,
        )
    };
}

/// Emit a CI project skipped event.
#[macro_export]
macro_rules! emit_ci_project_skipped {
    ($path:expr, $reason:expr) => {
        ::tracing::info!(
            target: "cuenv::ci",
            event_type = "ci.project_skipped",
            path = %$path,
            reason = %$reason,
        )
    };
}

/// Emit a CI task executing event.
#[macro_export]
macro_rules! emit_ci_task_executing {
    ($project:expr, $task:expr) => {
        ::tracing::info!(
            target: "cuenv::ci",
            event_type = "ci.task_executing",
            project = %$project,
            task = %$task,
        )
    };
}

/// Emit a CI task result event.
#[macro_export]
macro_rules! emit_ci_task_result {
    ($project:expr, $task:expr, $success:expr) => {
        ::tracing::info!(
            target: "cuenv::ci",
            event_type = "ci.task_result",
            project = %$project,
            task = %$task,
            success = $success,
        )
    };
    ($project:expr, $task:expr, $success:expr, $error:expr) => {
        ::tracing::info!(
            target: "cuenv::ci",
            event_type = "ci.task_result",
            project = %$project,
            task = %$task,
            success = $success,
            error = %$error,
        )
    };
}

/// Emit a CI report generated event.
#[macro_export]
macro_rules! emit_ci_report {
    ($path:expr) => {
        ::tracing::info!(
            target: "cuenv::ci",
            event_type = "ci.report_generated",
            path = %$path,
        )
    };
}

// Command Events

/// Emit a command started event.
#[macro_export]
macro_rules! emit_command_started {
    ($command:expr) => {
        ::tracing::info!(
            target: "cuenv::command",
            event_type = "command.started",
            command = %$command,
        )
    };
    ($command:expr, $args:expr) => {
        ::tracing::info!(
            target: "cuenv::command",
            event_type = "command.started",
            command = %$command,
            args = ?$args,
        )
    };
}

/// Emit a command progress event.
#[macro_export]
macro_rules! emit_command_progress {
    ($command:expr, $progress:expr, $message:expr) => {
        ::tracing::info!(
            target: "cuenv::command",
            event_type = "command.progress",
            command = %$command,
            progress = $progress,
            message = %$message,
        )
    };
}

/// Emit a command completed event.
#[macro_export]
macro_rules! emit_command_completed {
    ($command:expr, $success:expr, $duration_ms:expr) => {
        ::tracing::info!(
            target: "cuenv::command",
            event_type = "command.completed",
            command = %$command,
            success = $success,
            duration_ms = $duration_ms,
        )
    };
}

// Interactive Events

/// Emit a prompt requested event.
#[macro_export]
macro_rules! emit_prompt_requested {
    ($prompt_id:expr, $message:expr, $options:expr) => {
        ::tracing::info!(
            target: "cuenv::interactive",
            event_type = "interactive.prompt_requested",
            prompt_id = %$prompt_id,
            message = %$message,
            options = ?$options,
        )
    };
}

/// Emit a prompt resolved event.
#[macro_export]
macro_rules! emit_prompt_resolved {
    ($prompt_id:expr, $response:expr) => {
        ::tracing::info!(
            target: "cuenv::interactive",
            event_type = "interactive.prompt_resolved",
            prompt_id = %$prompt_id,
            response = %$response,
        )
    };
}

/// Emit a wait progress event.
#[macro_export]
macro_rules! emit_wait_progress {
    ($target:expr, $elapsed_secs:expr) => {
        ::tracing::info!(
            target: "cuenv::interactive",
            event_type = "interactive.wait_progress",
            task_name = %$target,
            elapsed_secs = $elapsed_secs,
        )
    };
}

// System Events

/// Emit a supervisor log event.
#[macro_export]
macro_rules! emit_supervisor_log {
    ($tag:expr, $message:expr) => {
        ::tracing::info!(
            target: "cuenv::system",
            event_type = "system.supervisor_log",
            tag = %$tag,
            message = %$message,
        )
    };
}

/// Emit a system shutdown event.
#[macro_export]
macro_rules! emit_shutdown {
    () => {
        ::tracing::info!(
            target: "cuenv::system",
            event_type = "system.shutdown",
        )
    };
}

// Output Events

/// Emit a stdout output event.
#[macro_export]
macro_rules! emit_stdout {
    ($content:expr) => {
        ::tracing::info!(
            target: "cuenv::output",
            event_type = "output.stdout",
            content = %$content,
        )
    };
}

/// Emit a stderr output event.
#[macro_export]
macro_rules! emit_stderr {
    ($content:expr) => {
        ::tracing::info!(
            target: "cuenv::output",
            event_type = "output.stderr",
            content = %$content,
        )
    };
}

/// Internal helpers used by the `emit_*!` macros.
///
/// Not part of the stable public API — exposed only because exported macros
/// must reference it via `$crate`.
#[doc(hidden)]
pub mod __macro_helpers {
    use crate::event::{CacheSkipReason, SkipReason};

    /// Serialize a [`CacheSkipReason`] to JSON for tracing field transport.
    #[must_use]
    pub fn encode_cache_skip_reason(reason: &CacheSkipReason) -> String {
        serde_json::to_string(reason).unwrap_or_default()
    }

    /// Serialize a [`SkipReason`] to JSON for tracing field transport.
    #[must_use]
    pub fn encode_skip_reason(reason: &SkipReason) -> String {
        serde_json::to_string(reason).unwrap_or_default()
    }
}

/// Print to stdout with automatic secret redaction (with newline).
///
/// Use this instead of `println!` when output might contain secrets.
/// This function applies `redact()` to the input before printing,
/// ensuring any registered secrets are replaced with `*_*`.
#[allow(clippy::print_stdout)]
pub fn println_redacted(content: &str) {
    println!("{}", redact(content));
}

/// Print to stdout with automatic secret redaction (no newline).
///
/// Use this instead of `print!` when output might contain secrets.
#[allow(clippy::print_stdout)]
pub fn print_redacted(content: &str) {
    print!("{}", redact(content));
}

#[cfg(test)]
#[allow(clippy::cognitive_complexity)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use tracing_subscriber::layer::SubscriberExt;

    fn with_test_subscriber(f: impl FnOnce()) {
        let (tx, _rx) = mpsc::unbounded_channel();
        let layer = CuenvEventLayer::new(tx);
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, f);
    }

    #[tokio::test]
    async fn test_task_macros_compile() {
        with_test_subscriber(|| {
            emit_task_started!("build", "cargo build", true);
            emit_task_cache_hit!("build", "abc123");
            emit_task_cache_miss!("test");
            emit_task_output!("build", "stdout", "output");
            emit_task_completed!("build", true, Some(0), 1000_u64);
            emit_task_group_started!("all", false, 3_usize);
            emit_task_group_completed!("all", true, 5000_u64);
        });
    }

    #[tokio::test]
    async fn test_task_macros_extended_forms_compile() {
        use crate::event::{CacheSkipReason, SkipReason, TaskKind};
        with_test_subscriber(|| {
            let parent: Option<&str> = Some("ci");
            emit_task_started!("ci.build", "cargo build", true, parent, "task");
            let _ = TaskKind::Task;
            emit_task_cache_hit!("ci.build", "abc123", parent);
            emit_task_cache_miss!("ci.test", parent);
            emit_task_cache_skipped!("ci.fmt", CacheSkipReason::EmptyInputs, parent);
            emit_task_queued!("ci.lint", 2_usize, parent);
            emit_task_skipped!(
                "ci.deploy",
                SkipReason::DependencyFailed {
                    dep: "ci.build".to_string()
                },
                parent
            );
            emit_task_retrying!("ci.flaky", 2_u32, 3_u32, parent);
            emit_task_output!("ci.build", "stdout", "out", parent);
            emit_task_completed!("ci.build", true, Some(0), 1000_u64, parent);
            emit_task_group_started!("ci", false, 3_usize, None::<&str>, Some(4_u32));
            emit_task_group_completed!(
                "ci",
                true,
                5000_u64,
                3_usize,
                0_usize,
                0_usize,
                None::<&str>
            );
        });
    }

    #[tokio::test]
    async fn test_ci_macros_compile() {
        with_test_subscriber(|| {
            emit_ci_context!("github", "push", "main");
            emit_ci_changed_files!(10_usize);
            emit_ci_projects_discovered!(3_usize);
            emit_ci_project_skipped!("/path", "no tasks");
            emit_ci_task_executing!("/path", "build");
            emit_ci_task_result!("/path", "build", true);
            emit_ci_task_result!("/path", "test", false, "assertion failed");
            emit_ci_report!("/path/report.json");
        });
    }

    #[tokio::test]
    async fn test_command_macros_compile() {
        with_test_subscriber(|| {
            emit_command_started!("env");
            emit_command_started!("task", vec!["build".to_string()]);
            emit_command_progress!("env", 0.5_f32, "loading");
            emit_command_completed!("env", true, 100_u64);
        });
    }

    #[tokio::test]
    async fn test_misc_macros_compile() {
        with_test_subscriber(|| {
            emit_prompt_requested!("p1", "Continue?", vec!["yes", "no"]);
            emit_prompt_resolved!("p1", "yes");
            emit_wait_progress!("hook", 5_u64);
            emit_supervisor_log!("supervisor", "started");
            emit_shutdown!();
            emit_stdout!("hello");
            emit_stderr!("error");
        });
    }

    #[tokio::test]
    async fn test_service_macros_compile() {
        with_test_subscriber(|| {
            emit_service_pending!("db");
            emit_service_starting!("db", "postgres -D /data");
            emit_service_output!("db", "stdout", "ready to accept connections");
            emit_service_ready!("db", 1200_u64);
            emit_service_ready_timeout!("db", 60000_u64);
            emit_service_restarting!("api", "crashed", 2_u32);
            emit_service_watch!("api", &["src/main.rs".to_string()]);
            emit_service_stopping!("db");
            emit_service_stopped!("db", Some(0));
            emit_service_failed!("api", "readiness timeout");
        });
    }
}
