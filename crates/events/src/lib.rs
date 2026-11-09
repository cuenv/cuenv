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
//! ```rust
//! use cuenv_events::bus;
//! use cuenv_events::{EventBus, EventCategory, TaskEvent, emit_task_started};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! // Install a sender and subscribe before emitting events.
//! let bus = EventBus::new();
//! let mut receiver = bus.subscribe();
//! bus::set_global_sender(bus.sender().expect("bus is open"));
//!
//! emit_task_started!("build", "cargo build", false);
//! let event = receiver.recv().await.expect("event was emitted");
//! assert!(matches!(event.category, EventCategory::Task(TaskEvent::Started { .. })));
//! bus::clear_global_sender();
//! # }
//! ```

#[doc(hidden)]
#[path = "macro_helpers.rs"]
pub mod __macro_helpers;
pub mod bus;
mod console;
pub mod event;
mod event_redaction;
pub mod layer;
mod macros;
pub mod metadata;
pub mod redacting_writer;
pub mod redaction;
pub mod renderers;

// Re-exports for convenience
pub use bus::{
    EventBus, EventReceiver, EventSender, SendError, clear_global_sender, emit, emit_with_source,
    global_sender, set_global_sender,
};
pub use console::{eprint_redacted, eprintln_redacted, print_redacted, println_redacted};
pub use event::{
    CacheSkipReason, CiEvent, CommandEvent, CuenvEvent, EventCategory, EventSource,
    InteractiveEvent, OutputEvent, RestartReason, ServiceEvent, SkipReason, Stream, SystemEvent,
    TaskEvent, TaskKind,
};
pub use layer::CuenvEventLayer;
pub use metadata::{MetadataContext, correlation_id, set_correlation_id};
pub use redacting_writer::{LogFormat, RedactingStderr, RedactingWriter};
pub use redaction::{
    REDACTED_PLACEHOLDER, redact, redact_cow, redact_free_form_json_text, redact_json_text,
    redact_json_value, redact_json_value_and_keys, register_secret, register_secrets,
};
#[cfg(feature = "spinner")]
pub use renderers::SpinnerRenderer;
pub use renderers::{CliRenderer, JsonRenderer};

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
