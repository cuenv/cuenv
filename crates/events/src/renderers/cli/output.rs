use super::CliRenderer;
use crate::event::OutputEvent;

impl CliRenderer {
    pub(super) fn render_output(&self, event: &OutputEvent) {
        let _ = &self.config; // Reserved for future output rendering options.
        match event {
            OutputEvent::Stdout { content } => {
                println!("{content}");
            }
            OutputEvent::Stderr { content } => {
                eprintln!("{content}");
            }
        }
    }
}
