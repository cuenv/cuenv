use super::{CliRenderer, cli_renderer};
use crate::event::{OutputEvent, Stream};

/// The stream and the text an output event is written as: its content with
/// every registered secret replaced.
pub(super) fn redacted_output(event: &OutputEvent) -> (Stream, String) {
    match event {
        OutputEvent::Stdout { content } => (Stream::Stdout, crate::redaction::redact(content)),
        OutputEvent::Stderr { content } => (Stream::Stderr, crate::redaction::redact(content)),
    }
}

impl CliRenderer {
    pub(super) fn render_output(event: &OutputEvent) {
        let (stream, content) = redacted_output(event);
        match stream {
            Stream::Stdout => cli_renderer::stdout_line(format_args!("{content}")),
            Stream::Stderr => cli_renderer::stderr_line(format_args!("{content}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redaction;
    #[test]
    fn stdout_and_stderr_content_never_carries_a_registered_secret() {
        redaction::test_support::with_clean_registry(|| {
            redaction::register_secret("render-secret-WXYZ");
            let (stream, text) = redacted_output(&OutputEvent::Stdout {
                content: "plan: token=render-secret-WXYZ".to_string(),
            });
            assert_eq!(stream, Stream::Stdout);
            assert_eq!(text, "plan: token=*_*");
            let (stream, text) = redacted_output(&OutputEvent::Stderr {
                content: "render-secret-WXYZ".to_string(),
            });
            assert_eq!(stream, Stream::Stderr);
            assert_eq!(text, "*_*");
        });
    }
}
