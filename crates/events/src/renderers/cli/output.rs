use super::{CliRenderer, stderr_line, stdout_line};
use crate::event::OutputEvent;

impl CliRenderer {
    pub(super) fn render_output(event: &OutputEvent) {
        match event {
            OutputEvent::Stdout { content } => {
                stdout_line(format_args!("{content}"));
            }
            OutputEvent::Stderr { content } => {
                stderr_line(format_args!("{content}"));
            }
        }
    }
}
