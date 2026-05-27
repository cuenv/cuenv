use super::{CliRenderer, stderr_line};
use crate::event::CommandEvent;

impl CliRenderer {
    pub(super) fn render_command(&self, event: &CommandEvent) {
        match event {
            CommandEvent::Started { command, .. } => {
                if self.config.verbose {
                    stderr_line(format_args!("Starting command: {command}"));
                }
            }
            CommandEvent::Progress {
                progress, message, ..
            } => {
                if self.config.verbose {
                    let pct = progress * 100.0;
                    stderr_line(format_args!("[{pct:.0}%] {message}"));
                }
            }
            CommandEvent::Completed {
                command,
                success,
                duration_ms,
            } => {
                if self.config.verbose {
                    let status = if *success { "completed" } else { "failed" };
                    stderr_line(format_args!(
                        "Command {command} {status} in {duration_ms}ms"
                    ));
                }
            }
        }
    }
}
