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
    EventBus, EventReceiver, EventSender, SendError, clear_global_sender, emit, emit_with_source,
    global_sender, set_global_sender,
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
//
// These macros are the canonical emit path. They construct a typed
// `CuenvEvent` and publish it via `cuenv_events::emit` against the
// process-wide `EventSender` installed by `set_global_sender`. If no
// sender has been installed (tests, library embeddings without a bus)
// the call is a no-op.
//
// The old tracing-based transport (`CuenvEventLayer` + `CuenvEventVisitor`)
// has been retired — events no longer round-trip through tracing fields.

/// Emit a task started event.
///
/// Two forms are supported:
/// - `emit_task_started!(name, command, hermetic)` — leaf task, no group.
/// - `emit_task_started!(name, command, hermetic, parent_group, task_kind)`
///   — leaf or group task, optionally inside a parent group.
///   `parent_group` is `Option<&str>` or `Option<String>`; `task_kind`
///   is a `&'static str` matching `TaskKind` (`"task"`, `"group"`, `"sequence"`).
#[macro_export]
macro_rules! emit_task_started {
    ($name:expr, $command:expr, $hermetic:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task($crate::TaskEvent::Started {
            name: ::std::string::ToString::to_string(&$name),
            command: ::std::string::ToString::to_string(&$command),
            hermetic: $hermetic,
            parent_group: ::std::option::Option::None,
            task_kind: <$crate::TaskKind as ::std::default::Default>::default(),
        }));
    }};
    ($name:expr, $command:expr, $hermetic:expr, $parent_group:expr, $task_kind:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task($crate::TaskEvent::Started {
            name: ::std::string::ToString::to_string(&$name),
            command: ::std::string::ToString::to_string(&$command),
            hermetic: $hermetic,
            parent_group: ($parent_group).map(|p| ::std::string::ToString::to_string(&p)),
            task_kind: $crate::__macro_helpers::parse_task_kind($task_kind),
        }));
    }};
}

/// Emit a task cache hit event.
#[macro_export]
macro_rules! emit_task_cache_hit {
    ($name:expr, $cache_key:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task($crate::TaskEvent::CacheHit {
            name: ::std::string::ToString::to_string(&$name),
            cache_key: ::std::string::ToString::to_string(&$cache_key),
            parent_group: ::std::option::Option::None,
        }));
    }};
    ($name:expr, $cache_key:expr, $parent_group:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task($crate::TaskEvent::CacheHit {
            name: ::std::string::ToString::to_string(&$name),
            cache_key: ::std::string::ToString::to_string(&$cache_key),
            parent_group: ($parent_group).map(|p| ::std::string::ToString::to_string(&p)),
        }));
    }};
}

/// Emit a task cache miss event.
#[macro_export]
macro_rules! emit_task_cache_miss {
    ($name:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task($crate::TaskEvent::CacheMiss {
            name: ::std::string::ToString::to_string(&$name),
            parent_group: ::std::option::Option::None,
        }));
    }};
    ($name:expr, $parent_group:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task($crate::TaskEvent::CacheMiss {
            name: ::std::string::ToString::to_string(&$name),
            parent_group: ($parent_group).map(|p| ::std::string::ToString::to_string(&p)),
        }));
    }};
}

/// Emit a task cache skipped event. `reason` is a [`CacheSkipReason`].
#[macro_export]
macro_rules! emit_task_cache_skipped {
    ($name:expr, $reason:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task(
            $crate::TaskEvent::CacheSkipped {
                name: ::std::string::ToString::to_string(&$name),
                parent_group: ::std::option::Option::None,
                reason: $reason,
            },
        ));
    }};
    ($name:expr, $reason:expr, $parent_group:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task(
            $crate::TaskEvent::CacheSkipped {
                name: ::std::string::ToString::to_string(&$name),
                parent_group: ($parent_group).map(|p| ::std::string::ToString::to_string(&p)),
                reason: $reason,
            },
        ));
    }};
}

/// Emit a task queued event.
#[macro_export]
macro_rules! emit_task_queued {
    ($name:expr, $queue_position:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task($crate::TaskEvent::Queued {
            name: ::std::string::ToString::to_string(&$name),
            parent_group: ::std::option::Option::None,
            queue_position: $queue_position,
        }));
    }};
    ($name:expr, $queue_position:expr, $parent_group:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task($crate::TaskEvent::Queued {
            name: ::std::string::ToString::to_string(&$name),
            parent_group: ($parent_group).map(|p| ::std::string::ToString::to_string(&p)),
            queue_position: $queue_position,
        }));
    }};
}

/// Emit a task skipped event. `reason` is a [`SkipReason`].
#[macro_export]
macro_rules! emit_task_skipped {
    ($name:expr, $reason:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task($crate::TaskEvent::Skipped {
            name: ::std::string::ToString::to_string(&$name),
            parent_group: ::std::option::Option::None,
            reason: $reason,
        }));
    }};
    ($name:expr, $reason:expr, $parent_group:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task($crate::TaskEvent::Skipped {
            name: ::std::string::ToString::to_string(&$name),
            parent_group: ($parent_group).map(|p| ::std::string::ToString::to_string(&p)),
            reason: $reason,
        }));
    }};
}

/// Emit a task retrying event.
#[macro_export]
macro_rules! emit_task_retrying {
    ($name:expr, $attempt:expr, $max_attempts:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task($crate::TaskEvent::Retrying {
            name: ::std::string::ToString::to_string(&$name),
            parent_group: ::std::option::Option::None,
            attempt: $attempt,
            max_attempts: $max_attempts,
        }));
    }};
    ($name:expr, $attempt:expr, $max_attempts:expr, $parent_group:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task($crate::TaskEvent::Retrying {
            name: ::std::string::ToString::to_string(&$name),
            parent_group: ($parent_group).map(|p| ::std::string::ToString::to_string(&p)),
            attempt: $attempt,
            max_attempts: $max_attempts,
        }));
    }};
}

/// Emit a task output event. `stream` is `"stdout"` or `"stderr"`.
#[macro_export]
macro_rules! emit_task_output {
    ($name:expr, $stream:expr, $content:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task($crate::TaskEvent::Output {
            name: ::std::string::ToString::to_string(&$name),
            stream: $crate::__macro_helpers::parse_stream($stream),
            content: ::std::string::ToString::to_string(&$content),
            parent_group: ::std::option::Option::None,
        }));
    }};
    ($name:expr, $stream:expr, $content:expr, $parent_group:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task($crate::TaskEvent::Output {
            name: ::std::string::ToString::to_string(&$name),
            stream: $crate::__macro_helpers::parse_stream($stream),
            content: ::std::string::ToString::to_string(&$content),
            parent_group: ($parent_group).map(|p| ::std::string::ToString::to_string(&p)),
        }));
    }};
}

/// Emit a task completed event.
#[macro_export]
macro_rules! emit_task_completed {
    ($name:expr, $success:expr, $exit_code:expr, $duration_ms:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task($crate::TaskEvent::Completed {
            name: ::std::string::ToString::to_string(&$name),
            success: $success,
            exit_code: $exit_code,
            duration_ms: $duration_ms,
            parent_group: ::std::option::Option::None,
        }));
    }};
    ($name:expr, $success:expr, $exit_code:expr, $duration_ms:expr, $parent_group:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task($crate::TaskEvent::Completed {
            name: ::std::string::ToString::to_string(&$name),
            success: $success,
            exit_code: $exit_code,
            duration_ms: $duration_ms,
            parent_group: ($parent_group).map(|p| ::std::string::ToString::to_string(&p)),
        }));
    }};
}

/// Emit a task group started event.
#[macro_export]
macro_rules! emit_task_group_started {
    ($name:expr, $sequential:expr, $task_count:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task(
            $crate::TaskEvent::GroupStarted {
                name: ::std::string::ToString::to_string(&$name),
                sequential: $sequential,
                task_count: $task_count,
                parent_group: ::std::option::Option::None,
                max_concurrency: ::std::option::Option::None,
            },
        ));
    }};
    ($name:expr, $sequential:expr, $task_count:expr, $parent_group:expr, $max_concurrency:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task(
            $crate::TaskEvent::GroupStarted {
                name: ::std::string::ToString::to_string(&$name),
                sequential: $sequential,
                task_count: $task_count,
                parent_group: ($parent_group).map(|p| ::std::string::ToString::to_string(&p)),
                max_concurrency: $max_concurrency,
            },
        ));
    }};
}

/// Emit a task group completed event.
#[macro_export]
macro_rules! emit_task_group_completed {
    ($name:expr, $success:expr, $duration_ms:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task(
            $crate::TaskEvent::GroupCompleted {
                name: ::std::string::ToString::to_string(&$name),
                success: $success,
                duration_ms: $duration_ms,
                parent_group: ::std::option::Option::None,
                succeeded: 0,
                failed: 0,
                skipped: 0,
            },
        ));
    }};
    ($name:expr, $success:expr, $duration_ms:expr, $succeeded:expr, $failed:expr, $skipped:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task(
            $crate::TaskEvent::GroupCompleted {
                name: ::std::string::ToString::to_string(&$name),
                success: $success,
                duration_ms: $duration_ms,
                parent_group: ::std::option::Option::None,
                succeeded: $succeeded,
                failed: $failed,
                skipped: $skipped,
            },
        ));
    }};
    ($name:expr, $success:expr, $duration_ms:expr, $succeeded:expr, $failed:expr, $skipped:expr, $parent_group:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Task(
            $crate::TaskEvent::GroupCompleted {
                name: ::std::string::ToString::to_string(&$name),
                success: $success,
                duration_ms: $duration_ms,
                parent_group: ($parent_group).map(|p| ::std::string::ToString::to_string(&p)),
                succeeded: $succeeded,
                failed: $failed,
                skipped: $skipped,
            },
        ));
    }};
}

// Service Events

/// Emit a service pending event.
#[macro_export]
macro_rules! emit_service_pending {
    ($name:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Service(
            $crate::ServiceEvent::Pending {
                name: ::std::string::ToString::to_string(&$name),
            },
        ));
    }};
}

/// Emit a service starting event.
#[macro_export]
macro_rules! emit_service_starting {
    ($name:expr, $command:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Service(
            $crate::ServiceEvent::Starting {
                name: ::std::string::ToString::to_string(&$name),
                command: ::std::string::ToString::to_string(&$command),
            },
        ));
    }};
}

/// Emit a service output event.
#[macro_export]
macro_rules! emit_service_output {
    ($name:expr, $stream:expr, $line:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Service(
            $crate::ServiceEvent::Output {
                name: ::std::string::ToString::to_string(&$name),
                stream: $crate::__macro_helpers::parse_stream($stream),
                line: ::std::string::ToString::to_string(&$line),
            },
        ));
    }};
}

/// Emit a service ready event.
#[macro_export]
macro_rules! emit_service_ready {
    ($name:expr, $after_ms:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Service(
            $crate::ServiceEvent::Ready {
                name: ::std::string::ToString::to_string(&$name),
                after_ms: $after_ms,
            },
        ));
    }};
}

/// Emit a service stopping event.
#[macro_export]
macro_rules! emit_service_stopping {
    ($name:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Service(
            $crate::ServiceEvent::Stopping {
                name: ::std::string::ToString::to_string(&$name),
            },
        ));
    }};
}

/// Emit a service stopped event.
#[macro_export]
macro_rules! emit_service_stopped {
    ($name:expr, $exit_code:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Service(
            $crate::ServiceEvent::Stopped {
                name: ::std::string::ToString::to_string(&$name),
                exit_code: $exit_code,
            },
        ));
    }};
}

/// Emit a service failed event.
#[macro_export]
macro_rules! emit_service_failed {
    ($name:expr, $error:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Service(
            $crate::ServiceEvent::Failed {
                name: ::std::string::ToString::to_string(&$name),
                error: ::std::string::ToString::to_string(&$error),
            },
        ));
    }};
}

/// Emit a service readiness timeout event.
#[macro_export]
macro_rules! emit_service_ready_timeout {
    ($name:expr, $after_ms:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Service(
            $crate::ServiceEvent::ReadyTimeout {
                name: ::std::string::ToString::to_string(&$name),
                after_ms: $after_ms,
            },
        ));
    }};
}

/// Emit a service restarting event. `reason` is a [`RestartReason`].
#[macro_export]
macro_rules! emit_service_restarting {
    ($name:expr, $reason:expr, $attempt:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Service(
            $crate::ServiceEvent::Restarting {
                name: ::std::string::ToString::to_string(&$name),
                reason: $crate::__macro_helpers::parse_restart_reason(&$reason),
                attempt: $attempt,
            },
        ));
    }};
}

/// Emit a service file watch event.
#[macro_export]
macro_rules! emit_service_watch {
    ($name:expr, $changed:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Service(
            $crate::ServiceEvent::Watch {
                name: ::std::string::ToString::to_string(&$name),
                changed: ($changed)
                    .iter()
                    .map(|p| ::std::string::ToString::to_string(p))
                    .collect(),
            },
        ));
    }};
}

// CI Events

/// Emit a CI context detected event.
#[macro_export]
macro_rules! emit_ci_context {
    ($provider:expr, $event_type:expr, $ref_name:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Ci(
            $crate::CiEvent::ContextDetected {
                provider: ::std::string::ToString::to_string(&$provider),
                event_type: ::std::string::ToString::to_string(&$event_type),
                ref_name: ::std::string::ToString::to_string(&$ref_name),
            },
        ));
    }};
}

/// Emit a CI changed files found event.
#[macro_export]
macro_rules! emit_ci_changed_files {
    ($count:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Ci(
            $crate::CiEvent::ChangedFilesFound { count: $count },
        ));
    }};
}

/// Emit a CI projects discovered event.
#[macro_export]
macro_rules! emit_ci_projects_discovered {
    ($count:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Ci(
            $crate::CiEvent::ProjectsDiscovered { count: $count },
        ));
    }};
}

/// Emit a CI project skipped event.
#[macro_export]
macro_rules! emit_ci_project_skipped {
    ($path:expr, $reason:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Ci($crate::CiEvent::ProjectSkipped {
            path: ::std::string::ToString::to_string(&$path),
            reason: ::std::string::ToString::to_string(&$reason),
        }));
    }};
}

/// Emit a CI task executing event.
#[macro_export]
macro_rules! emit_ci_task_executing {
    ($project:expr, $task:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Ci($crate::CiEvent::TaskExecuting {
            project: ::std::string::ToString::to_string(&$project),
            task: ::std::string::ToString::to_string(&$task),
        }));
    }};
}

/// Emit a CI task result event.
#[macro_export]
macro_rules! emit_ci_task_result {
    ($project:expr, $task:expr, $success:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Ci($crate::CiEvent::TaskResult {
            project: ::std::string::ToString::to_string(&$project),
            task: ::std::string::ToString::to_string(&$task),
            success: $success,
            error: ::std::option::Option::None,
        }));
    }};
    ($project:expr, $task:expr, $success:expr, $error:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Ci($crate::CiEvent::TaskResult {
            project: ::std::string::ToString::to_string(&$project),
            task: ::std::string::ToString::to_string(&$task),
            success: $success,
            error: ::std::option::Option::Some(::std::string::ToString::to_string(&$error)),
        }));
    }};
}

/// Emit a CI report generated event.
#[macro_export]
macro_rules! emit_ci_report {
    ($path:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Ci(
            $crate::CiEvent::ReportGenerated {
                path: ::std::string::ToString::to_string(&$path),
            },
        ));
    }};
}

// Command Events

/// Emit a command started event.
#[macro_export]
macro_rules! emit_command_started {
    ($command:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Command(
            $crate::CommandEvent::Started {
                command: ::std::string::ToString::to_string(&$command),
                args: ::std::vec::Vec::new(),
            },
        ));
    }};
    ($command:expr, $args:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Command(
            $crate::CommandEvent::Started {
                command: ::std::string::ToString::to_string(&$command),
                args: ($args)
                    .into_iter()
                    .map(|a| ::std::string::ToString::to_string(&a))
                    .collect(),
            },
        ));
    }};
}

/// Emit a command progress event.
#[macro_export]
macro_rules! emit_command_progress {
    ($command:expr, $progress:expr, $message:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Command(
            $crate::CommandEvent::Progress {
                command: ::std::string::ToString::to_string(&$command),
                progress: $progress,
                message: ::std::string::ToString::to_string(&$message),
            },
        ));
    }};
}

/// Emit a command completed event.
#[macro_export]
macro_rules! emit_command_completed {
    ($command:expr, $success:expr, $duration_ms:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Command(
            $crate::CommandEvent::Completed {
                command: ::std::string::ToString::to_string(&$command),
                success: $success,
                duration_ms: $duration_ms,
            },
        ));
    }};
}

// Interactive Events

/// Emit a prompt requested event.
#[macro_export]
macro_rules! emit_prompt_requested {
    ($prompt_id:expr, $message:expr, $options:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Interactive(
            $crate::InteractiveEvent::PromptRequested {
                prompt_id: ::std::string::ToString::to_string(&$prompt_id),
                message: ::std::string::ToString::to_string(&$message),
                options: ($options)
                    .into_iter()
                    .map(|o| ::std::string::ToString::to_string(&o))
                    .collect(),
            },
        ));
    }};
}

/// Emit a prompt resolved event.
#[macro_export]
macro_rules! emit_prompt_resolved {
    ($prompt_id:expr, $response:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Interactive(
            $crate::InteractiveEvent::PromptResolved {
                prompt_id: ::std::string::ToString::to_string(&$prompt_id),
                response: ::std::string::ToString::to_string(&$response),
            },
        ));
    }};
}

/// Emit a wait progress event.
#[macro_export]
macro_rules! emit_wait_progress {
    ($target:expr, $elapsed_secs:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Interactive(
            $crate::InteractiveEvent::WaitProgress {
                target: ::std::string::ToString::to_string(&$target),
                elapsed_secs: $elapsed_secs,
            },
        ));
    }};
}

// System Events

/// Emit a supervisor log event.
#[macro_export]
macro_rules! emit_supervisor_log {
    ($tag:expr, $message:expr) => {{
        let _ = $crate::emit($crate::EventCategory::System(
            $crate::SystemEvent::SupervisorLog {
                tag: ::std::string::ToString::to_string(&$tag),
                message: ::std::string::ToString::to_string(&$message),
            },
        ));
    }};
}

/// Emit a system shutdown event.
#[macro_export]
macro_rules! emit_shutdown {
    () => {{
        let _ = $crate::emit($crate::EventCategory::System($crate::SystemEvent::Shutdown));
    }};
}

// Output Events

/// Emit a stdout output event.
#[macro_export]
macro_rules! emit_stdout {
    ($content:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Output($crate::OutputEvent::Stdout {
            content: ::std::string::ToString::to_string(&$content),
        }));
    }};
}

/// Emit a stderr output event.
#[macro_export]
macro_rules! emit_stderr {
    ($content:expr) => {{
        let _ = $crate::emit($crate::EventCategory::Output($crate::OutputEvent::Stderr {
            content: ::std::string::ToString::to_string(&$content),
        }));
    }};
}

/// Internal helpers used by the `emit_*!` macros.
///
/// Not part of the stable public API — exposed only because exported macros
/// must reference it via `$crate`.
#[doc(hidden)]
pub mod __macro_helpers {
    use crate::event::{RestartReason, Stream, TaskKind};

    /// Map the string `task_kind` discriminator the old tracing macros
    /// accepted to the typed [`TaskKind`] used by the direct emit path.
    /// Unknown values default to [`TaskKind::Task`].
    #[must_use]
    pub fn parse_task_kind(kind: &str) -> TaskKind {
        match kind {
            "group" => TaskKind::Group,
            "sequence" => TaskKind::Sequence,
            _ => TaskKind::Task,
        }
    }

    /// Map the string `stream` discriminator to the typed [`Stream`] enum.
    /// Unknown values fall back to [`Stream::Stdout`].
    #[must_use]
    pub fn parse_stream(stream: &str) -> Stream {
        match stream {
            "stderr" => Stream::Stderr,
            _ => Stream::Stdout,
        }
    }

    /// Map a free-form `restart_reason` string to a typed
    /// [`RestartReason`]. Unknown values default to `Manual`.
    #[must_use]
    pub fn parse_restart_reason(reason: &(impl ::std::fmt::Display + ?Sized)) -> RestartReason {
        match reason.to_string().as_str() {
            "crashed" | "Crashed" => RestartReason::Crashed,
            "watch_triggered" | "WatchTriggered" => RestartReason::WatchTriggered,
            _ => RestartReason::Manual,
        }
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
