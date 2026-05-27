use super::{CliRenderer, flush_stderr, flush_stdout, stderr, stdout, stdout_line};
use crate::event::InteractiveEvent;

impl CliRenderer {
    pub(super) fn render_interactive(event: &InteractiveEvent) {
        match event {
            InteractiveEvent::PromptRequested {
                message, options, ..
            } => {
                stdout_line(format_args!("{message}"));
                for (i, option) in options.iter().enumerate() {
                    stdout_line(format_args!("  [{i}] {option}"));
                }
                stdout(format_args!("> "));
                flush_stdout();
            }
            InteractiveEvent::PromptResolved { .. } => {
                // Response handled elsewhere
            }
            InteractiveEvent::WaitProgress {
                target,
                elapsed_secs,
            } => {
                stderr(format_args!(
                    "\r\x1b[KWaiting for `{target}`... [{elapsed_secs}s]"
                ));
                flush_stderr();
            }
        }
    }
}
