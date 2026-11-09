use super::{CliRenderer, cli_renderer};
use crate::event::{ServiceEvent, Stream};

impl CliRenderer {
    pub(super) fn render_service(event: &ServiceEvent) {
        match event {
            ServiceEvent::Pending { name } => {
                cli_renderer::stderr_line(format_args!(
                    "> [{name}] pending (waiting for dependencies)"
                ));
            }
            ServiceEvent::Starting { name, command } => {
                cli_renderer::stderr_line(format_args!("> [{name}] starting: {command}"));
            }
            ServiceEvent::Output { name, stream, line } => match stream {
                Stream::Stdout => {
                    cli_renderer::stdout_line(format_args!("[{name}] {line}"));
                }
                Stream::Stderr => {
                    cli_renderer::stderr_line(format_args!("[{name}] {line}"));
                }
            },
            ServiceEvent::Ready { name, after_ms } => {
                cli_renderer::stderr_line(format_args!("> [{name}] ready ({after_ms}ms)"));
            }
            ServiceEvent::ReadyTimeout { name, after_ms } => {
                cli_renderer::stderr_line(format_args!(
                    "> [{name}] readiness timeout after {after_ms}ms"
                ));
            }
            ServiceEvent::Restarting {
                name,
                reason,
                attempt,
            } => {
                cli_renderer::stderr_line(format_args!(
                    "> [{name}] restarting (reason: {reason:?}, attempt: {attempt})"
                ));
            }
            ServiceEvent::Stopping { name } => {
                cli_renderer::stderr_line(format_args!("> [{name}] stopping"));
            }
            ServiceEvent::Stopped { name, exit_code } => {
                let code = exit_code.map_or_else(|| "signal".to_string(), |c| c.to_string());
                cli_renderer::stderr_line(format_args!("> [{name}] stopped (exit: {code})"));
            }
            ServiceEvent::Failed { name, error } => {
                cli_renderer::stderr_line(format_args!("> [{name}] FAILED: {error}"));
            }
            ServiceEvent::Watch { name, changed } => {
                cli_renderer::stderr_line(format_args!(
                    "> [{name}] files changed: {}",
                    changed.join(", ")
                ));
            }
        }
    }
}
