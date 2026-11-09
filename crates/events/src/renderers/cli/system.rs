use super::{CliRenderer, cli_renderer};
use crate::event::SystemEvent;

impl CliRenderer {
    pub(super) fn render_system(&self, event: &SystemEvent) {
        match event {
            SystemEvent::SupervisorLog { tag, message } => {
                cli_renderer::stderr_line(format_args!("[{tag}] {message}"));
            }
            SystemEvent::Shutdown => {
                if self.config.verbose {
                    cli_renderer::stderr_line(format_args!("System shutdown"));
                }
            }
            SystemEvent::EventGap { skipped } => {
                cli_renderer::stderr_line(format_args!(
                    "⚠  event bus lagged: {skipped} events dropped"
                ));
            }
        }
    }
}
