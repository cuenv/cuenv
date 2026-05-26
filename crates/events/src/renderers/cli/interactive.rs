use super::CliRenderer;
use crate::event::InteractiveEvent;
use std::io::{self, Write};

impl CliRenderer {
    pub(super) fn render_interactive(&self, event: &InteractiveEvent) {
        let _ = &self.config; // Reserved for future interactive rendering options.
        match event {
            InteractiveEvent::PromptRequested {
                message, options, ..
            } => {
                println!("{message}");
                for (i, option) in options.iter().enumerate() {
                    println!("  [{i}] {option}");
                }
                print!("> ");
                let _ = io::stdout().flush();
            }
            InteractiveEvent::PromptResolved { .. } => {
                // Response handled elsewhere
            }
            InteractiveEvent::WaitProgress {
                target,
                elapsed_secs,
            } => {
                eprint!("\r\x1b[KWaiting for `{target}`... [{elapsed_secs}s]");
                let _ = io::stderr().flush();
            }
        }
    }
}
