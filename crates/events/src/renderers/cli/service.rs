use super::CliRenderer;
use crate::event::{ServiceEvent, Stream};

impl CliRenderer {
    pub(super) fn render_service(&self, event: &ServiceEvent) {
        let _ = &self.config; // Reserved for future service rendering options.
        match event {
            ServiceEvent::Pending { name } => {
                eprintln!("> [{name}] pending (waiting for dependencies)");
            }
            ServiceEvent::Starting { name, command } => {
                eprintln!("> [{name}] starting: {command}");
            }
            ServiceEvent::Output { name, stream, line } => match stream {
                Stream::Stdout => {
                    println!("[{name}] {line}");
                }
                Stream::Stderr => {
                    eprintln!("[{name}] {line}");
                }
            },
            ServiceEvent::Ready { name, after_ms } => {
                eprintln!("> [{name}] ready ({after_ms}ms)");
            }
            ServiceEvent::ReadyTimeout { name, after_ms } => {
                eprintln!("> [{name}] readiness timeout after {after_ms}ms");
            }
            ServiceEvent::Restarting {
                name,
                reason,
                attempt,
            } => {
                eprintln!("> [{name}] restarting (reason: {reason:?}, attempt: {attempt})");
            }
            ServiceEvent::Stopping { name } => {
                eprintln!("> [{name}] stopping");
            }
            ServiceEvent::Stopped { name, exit_code } => {
                let code = exit_code.map_or_else(|| "signal".to_string(), |c| c.to_string());
                eprintln!("> [{name}] stopped (exit: {code})");
            }
            ServiceEvent::Failed { name, error } => {
                eprintln!("> [{name}] FAILED: {error}");
            }
            ServiceEvent::Watch { name, changed } => {
                eprintln!("> [{name}] files changed: {}", changed.join(", "));
            }
        }
    }
}
