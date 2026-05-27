use super::{CliRenderer, stderr_line};
use crate::event::SystemEvent;

impl CliRenderer {
    pub(super) fn render_system(&self, event: &SystemEvent) {
        match event {
            SystemEvent::SupervisorLog { tag, message } => {
                stderr_line(format_args!("[{tag}] {message}"));
            }
            SystemEvent::Shutdown => {
                if self.config.verbose {
                    stderr_line(format_args!("System shutdown"));
                }
            }
            SystemEvent::EventGap { skipped } => {
                stderr_line(format_args!(
                    "⚠  event bus lagged: {skipped} events dropped"
                ));
            }
        }
    }
}
