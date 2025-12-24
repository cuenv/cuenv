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
}
