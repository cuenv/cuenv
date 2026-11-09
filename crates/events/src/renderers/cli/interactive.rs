use super::{CliRenderer, cli_renderer};
use crate::event::InteractiveEvent;

impl CliRenderer {
    pub(super) fn render_interactive(event: &InteractiveEvent) {
        match event {
            InteractiveEvent::PromptRequested {
                message, options, ..
            } => {
                cli_renderer::stdout_line(format_args!("{message}"));
                for (i, option) in options.iter().enumerate() {
                    cli_renderer::stdout_line(format_args!("  [{i}] {option}"));
                }
                cli_renderer::stdout(format_args!("> "));
                cli_renderer::flush_stdout();
            }
            InteractiveEvent::PromptResolved { .. } => {
                // Response handled elsewhere
            }
            InteractiveEvent::WaitProgress {
                target,
                elapsed_secs,
            } => {
                cli_renderer::stderr(format_args!(
                    "\r\x1b[KWaiting for `{target}`... [{elapsed_secs}s]"
                ));
                cli_renderer::flush_stderr();
            }
        }
    }
}
