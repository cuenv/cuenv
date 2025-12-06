//! Custom tracing Layer for capturing cuenv events.
//!
//! This layer intercepts tracing events with specific targets and fields,
//! converts them to `CuenvEvent` instances, and sends them to the `EventBus`.

// These casts are intentional for tracing field extraction - values come from trusted sources
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]

use crate::event::{
    CiEvent, CommandEvent, CuenvEvent, EventCategory, EventSource, InteractiveEvent, OutputEvent,
    Stream, SystemEvent, TaskEvent,
};
use crate::metadata::correlation_id;
use tokio::sync::mpsc;
use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// A tracing Layer that captures cuenv-specific events.
///
/// Events are identified by their `target` (must start with "cuenv")
/// and an `event_type` field that specifies the event category.
pub struct CuenvEventLayer {
    sender: mpsc::UnboundedSender<CuenvEvent>,
}

impl CuenvEventLayer {
    /// Create a new layer that sends events to the given channel.
    #[must_use]
    pub fn new(sender: mpsc::UnboundedSender<CuenvEvent>) -> Self {
        Self { sender }
    }
}

impl<S> Layer<S> for CuenvEventLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let target = meta.target();

        // Only capture events with cuenv target
        if !target.starts_with("cuenv") {
            return;
        }

        // Extract fields using visitor pattern
        let mut visitor = CuenvEventVisitor::new(target);
        event.record(&mut visitor);

        // Build and send event if it has required fields
        if let Some(cuenv_event) = visitor.build() {
            let _ = self.sender.send(cuenv_event);
        }
    }
}

/// Visitor for extracting typed fields from tracing events.
struct CuenvEventVisitor {
    target: String,
    event_type: Option<String>,

    // Task event fields
    task_name: Option<String>,
    command: Option<String>,
    hermetic: Option<bool>,
    cache_key: Option<String>,
    stream: Option<Stream>,
    content: Option<String>,
    success: Option<bool>,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    sequential: Option<bool>,
    task_count: Option<usize>,

    // CI event fields
    provider: Option<String>,
    event_type_ci: Option<String>,
    ref_name: Option<String>,
    count: Option<usize>,
    path: Option<String>,
    project: Option<String>,
    task: Option<String>,
    reason: Option<String>,
    error: Option<String>,

    // Command event fields
    args: Option<Vec<String>>,
    progress: Option<f32>,
    message: Option<String>,

    // Interactive event fields
    prompt_id: Option<String>,
    options: Option<Vec<String>>,
    response: Option<String>,
    elapsed_secs: Option<u64>,

    // System event fields
    tag: Option<String>,
}

impl CuenvEventVisitor {
    fn new(target: &str) -> Self {
        Self {
            target: target.to_string(),
            event_type: None,
            task_name: None,
            command: None,
            hermetic: None,
            cache_key: None,
            stream: None,
            content: None,
            success: None,
            exit_code: None,
            duration_ms: None,
            sequential: None,
            task_count: None,
            provider: None,
            event_type_ci: None,
            ref_name: None,
            count: None,
            path: None,
            project: None,
            task: None,
            reason: None,
            error: None,
            args: None,
            progress: None,
            message: None,
            prompt_id: None,
            options: None,
            response: None,
            elapsed_secs: None,
            tag: None,
        }
    }

    fn build(self) -> Option<CuenvEvent> {
        let event_type = self.event_type.as_deref()?;
        let source = EventSource::new(&self.target);
        let correlation = correlation_id();

        let category = match event_type {
            // Task events
            "task.started" => EventCategory::Task(TaskEvent::Started {
                name: self.task_name?,
                command: self.command?,
                hermetic: self.hermetic.unwrap_or(false),
            }),
            "task.cache_hit" => EventCategory::Task(TaskEvent::CacheHit {
                name: self.task_name?,
                cache_key: self.cache_key?,
            }),
            "task.cache_miss" => EventCategory::Task(TaskEvent::CacheMiss {
                name: self.task_name?,
            }),
            "task.output" => EventCategory::Task(TaskEvent::Output {
                name: self.task_name?,
                stream: self.stream.unwrap_or(Stream::Stdout),
                content: self.content?,
            }),
            "task.completed" => EventCategory::Task(TaskEvent::Completed {
                name: self.task_name?,
                success: self.success?,
                exit_code: self.exit_code,
                duration_ms: self.duration_ms.unwrap_or(0),
            }),
            "task.group_started" => EventCategory::Task(TaskEvent::GroupStarted {
                name: self.task_name?,
                sequential: self.sequential.unwrap_or(false),
                task_count: self.task_count.unwrap_or(0),
            }),
            "task.group_completed" => EventCategory::Task(TaskEvent::GroupCompleted {
                name: self.task_name?,
                success: self.success?,
                duration_ms: self.duration_ms.unwrap_or(0),
            }),

            // CI events
            "ci.context_detected" => EventCategory::Ci(CiEvent::ContextDetected {
                provider: self.provider?,
                event_type: self.event_type_ci?,
                ref_name: self.ref_name?,
            }),
            "ci.changed_files" => {
                EventCategory::Ci(CiEvent::ChangedFilesFound { count: self.count? })
            }
            "ci.projects_discovered" => {
                EventCategory::Ci(CiEvent::ProjectsDiscovered { count: self.count? })
            }
            "ci.project_skipped" => EventCategory::Ci(CiEvent::ProjectSkipped {
                path: self.path?,
                reason: self.reason?,
            }),
            "ci.task_executing" => EventCategory::Ci(CiEvent::TaskExecuting {
                project: self.project?,
                task: self.task?,
            }),
            "ci.task_result" => EventCategory::Ci(CiEvent::TaskResult {
                project: self.project?,
                task: self.task?,
                success: self.success?,
                error: self.error,
            }),
            "ci.report_generated" => {
                EventCategory::Ci(CiEvent::ReportGenerated { path: self.path? })
            }

            // Command events
            "command.started" => EventCategory::Command(CommandEvent::Started {
                command: self.command?,
                args: self.args.unwrap_or_default(),
            }),
            "command.progress" => EventCategory::Command(CommandEvent::Progress {
                command: self.command?,
                progress: self.progress?,
                message: self.message?,
            }),
            "command.completed" => EventCategory::Command(CommandEvent::Completed {
                command: self.command?,
                success: self.success?,
                duration_ms: self.duration_ms.unwrap_or(0),
            }),

            // Interactive events
            "interactive.prompt_requested" => {
                EventCategory::Interactive(InteractiveEvent::PromptRequested {
                    prompt_id: self.prompt_id?,
                    message: self.message?,
                    options: self.options.unwrap_or_default(),
                })
            }
            "interactive.prompt_resolved" => {
                EventCategory::Interactive(InteractiveEvent::PromptResolved {
                    prompt_id: self.prompt_id?,
                    response: self.response?,
                })
            }
            "interactive.wait_progress" => {
                EventCategory::Interactive(InteractiveEvent::WaitProgress {
                    target: self.task_name.or(self.path)?,
                    elapsed_secs: self.elapsed_secs?,
                })
            }

            // System events
            "system.supervisor_log" => EventCategory::System(SystemEvent::SupervisorLog {
                tag: self.tag?,
                message: self.message?,
            }),
            "system.shutdown" => EventCategory::System(SystemEvent::Shutdown),

            // Output events
            "output.stdout" => EventCategory::Output(OutputEvent::Stdout {
                content: self.content?,
            }),
            "output.stderr" => EventCategory::Output(OutputEvent::Stderr {
                content: self.content?,
            }),

            _ => return None,
        };

        Some(CuenvEvent::new(correlation, source, category))
    }
}

impl Visit for CuenvEventVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "event_type" => self.event_type = Some(value.to_string()),
            "task_name" | "name" => self.task_name = Some(value.to_string()),
            "command" | "cmd" => self.command = Some(value.to_string()),
            "cache_key" => self.cache_key = Some(value.to_string()),
            "content" => self.content = Some(value.to_string()),
            "provider" => self.provider = Some(value.to_string()),
            "ci_event_type" => self.event_type_ci = Some(value.to_string()),
            "ref_name" => self.ref_name = Some(value.to_string()),
            "path" => self.path = Some(value.to_string()),
            "project" => self.project = Some(value.to_string()),
            "task" => self.task = Some(value.to_string()),
            "reason" => self.reason = Some(value.to_string()),
            "error" => self.error = Some(value.to_string()),
            "message" => self.message = Some(value.to_string()),
            "prompt_id" => self.prompt_id = Some(value.to_string()),
            "response" => self.response = Some(value.to_string()),
            "tag" => self.tag = Some(value.to_string()),
            "stream" => {
                self.stream = match value {
                    "stdout" => Some(Stream::Stdout),
                    "stderr" => Some(Stream::Stderr),
                    _ => None,
                };
            }
            _ => {}
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        match field.name() {
            "exit_code" => self.exit_code = Some(value as i32),
            "duration_ms" => self.duration_ms = Some(value as u64),
            "count" => self.count = Some(value as usize),
            "task_count" => self.task_count = Some(value as usize),
            "elapsed_secs" => self.elapsed_secs = Some(value as u64),
            _ => {}
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "duration_ms" => self.duration_ms = Some(value),
            "count" => self.count = Some(value as usize),
            "task_count" => self.task_count = Some(value as usize),
            "elapsed_secs" => self.elapsed_secs = Some(value),
            _ => {}
        }
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        if field.name() == "progress" {
            self.progress = Some(value as f32);
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        match field.name() {
            "hermetic" => self.hermetic = Some(value),
            "success" => self.success = Some(value),
            "sequential" => self.sequential = Some(value),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // Handle debug-formatted fields as strings
        let value_str = format!("{value:?}");
        match field.name() {
            "args" => {
                // Try to parse as JSON array
                if let Ok(args) = serde_json::from_str::<Vec<String>>(&value_str) {
                    self.args = Some(args);
                }
            }
            "options" => {
                if let Ok(options) = serde_json::from_str::<Vec<String>>(&value_str) {
                    self.options = Some(options);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use tracing_subscriber::layer::SubscriberExt;

    #[tokio::test]
    async fn test_layer_captures_cuenv_events() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let layer = CuenvEventLayer::new(tx);

        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "cuenv::output",
                event_type = "output.stdout",
                content = "test output",
                "Test event"
            );
        });

        let event = rx.recv().await.unwrap();
        match event.category {
            EventCategory::Output(OutputEvent::Stdout { content }) => {
                assert_eq!(content, "test output");
            }
            _ => panic!("Expected stdout output event"),
        }
    }

    #[tokio::test]
    async fn test_layer_ignores_non_cuenv_events() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let layer = CuenvEventLayer::new(tx);

        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "other::target",
                event_type = "output.stdout",
                content = "should be ignored",
                "Other event"
            );
        });

        // Give a moment for any event to be sent
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_layer_captures_task_events() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let layer = CuenvEventLayer::new(tx);

        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "cuenv::task",
                event_type = "task.started",
                task_name = "build",
                command = "cargo build",
                hermetic = true,
                "Task started"
            );
        });

        let event = rx.recv().await.unwrap();
        match event.category {
            EventCategory::Task(TaskEvent::Started {
                name,
                command,
                hermetic,
            }) => {
                assert_eq!(name, "build");
                assert_eq!(command, "cargo build");
                assert!(hermetic);
            }
            _ => panic!("Expected task started event"),
        }
    }
}
