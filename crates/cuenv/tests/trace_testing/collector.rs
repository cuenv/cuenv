//! Span collector layer for testing
//!
//! Captures tracing spans for later verification in tests.

use std::sync::{Arc, Mutex};
use tracing::Subscriber;
use tracing::span::{Attributes, Id};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// A captured span with its metadata
#[derive(Debug, Clone)]
pub struct CapturedSpan {
    /// The span name (from `#[instrument(name = "...")]`)
    pub name: String,
    /// The target module (e.g., "cuenv::commands::task")
    pub target: String,
    /// The tracing level (INFO, DEBUG, etc.)
    pub level: tracing::Level,
    /// Captured field values as (key, value) pairs
    pub fields: Vec<(String, String)>,
}

/// A tracing layer that collects spans for test assertions
pub struct SpanCollector {
    spans: Arc<Mutex<Vec<CapturedSpan>>>,
}

impl SpanCollector {
    /// Create a new span collector and return both the layer and a handle to the collected spans
    pub fn new() -> (Self, Arc<Mutex<Vec<CapturedSpan>>>) {
        let spans = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                spans: spans.clone(),
            },
            spans,
        )
    }
}

/// Field visitor that captures field values as strings
struct FieldVisitor {
    fields: Vec<(String, String)>,
}

impl FieldVisitor {
    fn new() -> Self {
        Self { fields: Vec::new() }
    }
}

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.fields
            .push((field.name().to_string(), format!("{:?}", value)));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }
}

impl<S: Subscriber> Layer<S> for SpanCollector {
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        let mut visitor = FieldVisitor::new();
        attrs.record(&mut visitor);

        let span = CapturedSpan {
            name: attrs.metadata().name().to_string(),
            target: attrs.metadata().target().to_string(),
            level: *attrs.metadata().level(),
            fields: visitor.fields,
        };

        if let Ok(mut spans) = self.spans.lock() {
            spans.push(span);
        }
    }
}
