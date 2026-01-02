//! JSON renderer for cuenv events.
//!
//! Renders events as JSON lines for machine consumption.
//! This module is allowed to use println! as it's the output layer.

#![allow(clippy::print_stdout)]

use crate::bus::EventReceiver;
use crate::event::CuenvEvent;

/// JSON renderer that outputs events as JSON lines.
#[derive(Debug, Default)]
pub struct JsonRenderer {
    /// Whether to pretty-print JSON.
    pretty: bool,
}

impl JsonRenderer {
    /// Create a new JSON renderer with compact output.
    #[must_use]
    pub const fn new() -> Self {
        Self { pretty: false }
    }

    /// Create a new JSON renderer with pretty-printed output.
    #[must_use]
    pub const fn pretty() -> Self {
        Self { pretty: true }
    }

    /// Run the renderer, consuming events from the receiver.
    pub async fn run(self, mut receiver: EventReceiver) {
        while let Some(event) = receiver.recv().await {
            self.render(&event);
        }
    }

    /// Render a single event as JSON.
    pub fn render(&self, event: &CuenvEvent) {
        let json = if self.pretty {
            serde_json::to_string_pretty(event)
        } else {
            serde_json::to_string(event)
        };

        if let Ok(json) = json {
            println!("{json}");
        }
    }

    /// Render a single event to a string (for testing).
    #[must_use]
    pub fn render_to_string(&self, event: &CuenvEvent) -> Option<String> {
        if self.pretty {
            serde_json::to_string_pretty(event).ok()
        } else {
            serde_json::to_string(event).ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventCategory, EventSource, OutputEvent};
    use uuid::Uuid;

    fn create_test_event() -> CuenvEvent {
        CuenvEvent::new(
            Uuid::nil(),
            EventSource::new("test::target"),
            EventCategory::Output(OutputEvent::Stdout {
                content: "Test message".to_string(),
            }),
        )
    }

    #[test]
    fn test_json_renderer_new_is_compact() {
        let renderer = JsonRenderer::new();
        assert!(!renderer.pretty);
    }

    #[test]
    fn test_json_renderer_pretty() {
        let renderer = JsonRenderer::pretty();
        assert!(renderer.pretty);
    }

    #[test]
    fn test_json_renderer_default_is_compact() {
        let renderer = JsonRenderer::default();
        assert!(!renderer.pretty);
    }

    #[test]
    fn test_json_renderer_debug() {
        let renderer = JsonRenderer::new();
        let debug = format!("{renderer:?}");
        assert!(debug.contains("JsonRenderer"));
        assert!(debug.contains("pretty"));
    }

    #[test]
    fn test_render_to_string_compact() {
        let renderer = JsonRenderer::new();
        let event = create_test_event();

        let json = renderer.render_to_string(&event);
        assert!(json.is_some());

        let json_str = json.unwrap();
        // Compact output should not have newlines in the middle
        assert!(!json_str.contains("\n  "));
        // Should contain expected fields
        assert!(json_str.contains("\"id\""));
        assert!(json_str.contains("\"correlation_id\""));
        assert!(json_str.contains("test::target"));
        assert!(json_str.contains("Test message"));
    }

    #[test]
    fn test_render_to_string_pretty() {
        let renderer = JsonRenderer::pretty();
        let event = create_test_event();

        let json = renderer.render_to_string(&event);
        assert!(json.is_some());

        let json_str = json.unwrap();
        // Pretty output should have newlines and indentation
        assert!(json_str.contains('\n'));
        assert!(json_str.contains("  "));
        // Should still contain expected fields
        assert!(json_str.contains("\"id\""));
        assert!(json_str.contains("test::target"));
    }

    #[test]
    fn test_render_to_string_is_valid_json() {
        let renderer = JsonRenderer::new();
        let event = create_test_event();

        let json_str = renderer.render_to_string(&event).unwrap();

        // Should be valid JSON that can be parsed back
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert!(parsed.is_object());
        assert!(parsed.get("id").is_some());
        assert!(parsed.get("timestamp").is_some());
        assert!(parsed.get("source").is_some());
        assert!(parsed.get("category").is_some());
    }

    #[test]
    fn test_render_to_string_pretty_is_valid_json() {
        let renderer = JsonRenderer::pretty();
        let event = create_test_event();

        let json_str = renderer.render_to_string(&event).unwrap();

        // Pretty-printed JSON should also be valid
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert!(parsed.is_object());
    }
}
