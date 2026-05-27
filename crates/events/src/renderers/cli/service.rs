use super::{CliRenderer, stderr_line, stdout_line};
use crate::event::{ServiceEvent, Stream};

impl CliRenderer {
    pub(super) fn render_service(&self, event: &ServiceEvent) {
        let _ = &self.config; // Reserved for future service rendering options.
        match event {
            ServiceEvent::Pending { name } => {
                stderr_line(format_args!(
                    "> [{name}] pending (waiting for dependencies)"
                ));
            }
            ServiceEvent::Starting { name, command } => {
                stderr_line(format_args!("> [{name}] starting: {command}"));
            }
            ServiceEvent::Output { name, stream, line } => match stream {
                Stream::Stdout => {
                    stdout_line(format_args!("[{name}] {line}"));
                }
                Stream::Stderr => {
                    stderr_line(format_args!("[{name}] {line}"));
                }
            },
            ServiceEvent::Ready { name, after_ms } => {
                stderr_line(format_args!("> [{name}] ready ({after_ms}ms)"));
            }
            ServiceEvent::ReadyTimeout { name, after_ms } => {
                stderr_line(format_args!(
                    "> [{name}] readiness timeout after {after_ms}ms"
                ));
            }
            ServiceEvent::Restarting {
                name,
                reason,
                attempt,
            } => {
                stderr_line(format_args!(
                    "> [{name}] restarting (reason: {reason:?}, attempt: {attempt})"
                ));
            }
            ServiceEvent::Stopping { name } => {
                stderr_line(format_args!("> [{name}] stopping"));
            }
            ServiceEvent::Stopped { name, exit_code } => {
                let code = exit_code.map_or_else(|| "signal".to_string(), |c| c.to_string());
                stderr_line(format_args!("> [{name}] stopped (exit: {code})"));
            }
            ServiceEvent::Failed { name, error } => {
                stderr_line(format_args!("> [{name}] FAILED: {error}"));
            }
            ServiceEvent::Watch { name, changed } => {
                stderr_line(format_args!(
                    "> [{name}] files changed: {}",
                    changed.join(", ")
                ));
            }
        }
    }
}
