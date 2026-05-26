use super::CliRenderer;
use crate::event::SystemEvent;

impl CliRenderer {
    pub(super) fn render_system(&self, event: &SystemEvent) {
        match event {
            SystemEvent::SupervisorLog { tag, message } => {
                eprintln!("[{tag}] {message}");
            }
            SystemEvent::Shutdown => {
                if self.config.verbose {
                    eprintln!("System shutdown");
                }
            }
            SystemEvent::EventGap { skipped } => {
                eprintln!("⚠  event bus lagged: {skipped} events dropped");
            }
        }
    }
}
