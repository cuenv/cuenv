//! Structured event system for cuenv.
//!
//! This crate provides a unified event system that enables multiple UI frontends
//! (CLI, TUI, Web) to subscribe to a single event stream. Events are emitted
//! through typed macros into a process-wide event sender; a compatibility
//! tracing layer remains available for older structured tracing producers.
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
mod macros;
pub mod metadata;
pub mod redaction;
pub mod renderers;

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

/// Print to stderr with automatic secret redaction (with newline).
///
/// Use this instead of `eprintln!` when output might contain secrets.
#[allow(clippy::print_stderr)]
pub fn eprintln_redacted(content: &str) {
    eprintln!("{}", redact(content));
}

/// Print to stderr with automatic secret redaction (no newline).
///
/// Use this instead of `eprint!` when output might contain secrets.
#[allow(clippy::print_stderr)]
pub fn eprint_redacted(content: &str) {
    eprint!("{}", redact(content));
}

#[cfg(test)]
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
