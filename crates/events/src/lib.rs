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

// Re-exports for convenience
pub use bus::{EventBus, EventReceiver, EventSender, SendError};
pub use event::{
    CiEvent, CommandEvent, CuenvEvent, EventCategory, EventSource, InteractiveEvent, OutputEvent,
    Stream, SystemEvent, TaskEvent,
};
pub use layer::CuenvEventLayer;
pub use metadata::{MetadataContext, correlation_id, set_correlation_id};
pub use redaction::{REDACTED_PLACEHOLDER, redact, register_secret, register_secrets};
pub use renderers::{CliRenderer, JsonRenderer};

// ============================================================================
// Emit Macros
// ============================================================================

/// Emit a task started event.
///
/// # Example
/// ```rust,ignore
/// emit_task_started!("build", "cargo build", true);
/// ```
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
}

/// Emit a task cache hit event.
///
/// # Example
/// ```rust,ignore
/// emit_task_cache_hit!("build", "abc123");
/// ```
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
}

/// Emit a task cache miss event.
#[macro_export]
macro_rules! emit_task_cache_miss {
    ($name:expr) => {
        ::tracing::info!(
            target: "cuenv::task",
            event_type = "task.cache_miss",
            task_name = %$name,
        )
    };
}

/// Emit a task output event.
///
/// # Example
/// ```rust,ignore
/// emit_task_output!("build", "stdout", "Compiling...");
/// ```
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
}

/// Emit a task completed event.
///
/// # Example
/// ```rust,ignore
/// emit_task_completed!("build", true, Some(0), 1234);
/// ```
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
}

/// Emit a task group started event.
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
}

/// Emit a task group completed event.
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
}
