//! Custom tracing Layer for capturing cuenv events.
//!
//! This layer intercepts tracing events with specific targets and fields,
//! converts them to `CuenvEvent` instances, and sends them to the `EventBus`.

use crate::event::CuenvEvent;
use crate::layer::visitor::CuenvEventVisitor;
use tokio::sync::mpsc;
use tracing::Subscriber;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

mod visitor;

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
    pub const fn new(sender: mpsc::UnboundedSender<CuenvEvent>) -> Self {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{CacheSkipReason, EventCategory, OutputEvent, TaskEvent, TaskKind};
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
                ..
            }) => {
                assert_eq!(name, "build");
                assert_eq!(command, "cargo build");
                assert!(hermetic);
            }
            _ => panic!("Expected task started event"),
        }
    }

    #[tokio::test]
    async fn test_layer_extracts_parent_group_and_task_kind() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let layer = CuenvEventLayer::new(tx);
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let parent: Option<&str> = Some("ci");
            tracing::info!(
                target: "cuenv::task",
                event_type = "task.started",
                task_name = "ci.build",
                command = "cargo build",
                hermetic = true,
                parent_group = ?parent,
                task_kind = "task",
                "Task started in group"
            );
        });

        let event = rx.recv().await.unwrap();
        match event.category {
            EventCategory::Task(TaskEvent::Started {
                name,
                parent_group,
                task_kind,
                ..
            }) => {
                assert_eq!(name, "ci.build");
                assert_eq!(parent_group.as_deref(), Some("ci"));
                assert_eq!(task_kind, TaskKind::Task);
            }
            other => panic!("Expected task started event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_layer_extracts_cache_skipped() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let layer = CuenvEventLayer::new(tx);
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let reason = CacheSkipReason::EmptyInputs;
            let encoded = serde_json::to_string(&reason).unwrap();
            tracing::info!(
                target: "cuenv::task",
                event_type = "task.cache_skipped",
                task_name = "fmt",
                cache_skip_reason = %encoded,
                "skipped"
            );
        });

        let event = rx.recv().await.unwrap();
        match event.category {
            EventCategory::Task(TaskEvent::CacheSkipped { name, reason, .. }) => {
                assert_eq!(name, "fmt");
                assert_eq!(reason, CacheSkipReason::EmptyInputs);
            }
            other => panic!("Expected cache skipped event, got {other:?}"),
        }
    }
}
