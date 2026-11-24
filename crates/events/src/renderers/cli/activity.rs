use super::{CliRenderer, cli_renderer};
use crate::event::{ActivityEvent, ActivityOutcome};
#[cfg(feature = "spinner")]
use std::io::{self, IsTerminal};

impl CliRenderer {
    pub(super) fn render_activity(&self, event: &ActivityEvent) {
        #[cfg(feature = "spinner")]
        if self.render_activity_with_spinner(event) {
            return;
        }
        render_activity_plain(event);
    }

    /// Draw the activity as a spinner when stderr, where indicatif draws, is
    /// a terminal. Returns `false` when the plain renderer must be used.
    #[cfg(feature = "spinner")]
    fn render_activity_with_spinner(&self, event: &ActivityEvent) -> bool {
        if !io::stderr().is_terminal() {
            return false;
        }
        let Some(spinner) = self.spinner.as_ref() else {
            return false;
        };
        let Ok(mut guard) = spinner.lock() else {
            return false;
        };
        guard.apply_activity(event);
        true
    }
}

/// Plain stderr lines for CI logs and redirected output.
fn render_activity_plain(event: &ActivityEvent) {
    match event {
        ActivityEvent::Started { message, .. } => {
            cli_renderer::stderr_line(format_args!("{message}..."));
        }
        ActivityEvent::Progress { detail, .. } => {
            cli_renderer::stderr_line(format_args!("  {detail}"));
        }
        ActivityEvent::Completed {
            message,
            outcome,
            duration_ms,
            ..
        } => {
            cli_renderer::stderr_line(format_args!(
                "{}",
                completion_line(message, *outcome, *duration_ms)
            ));
        }
    }
}

/// The line left behind when an activity ends.
pub fn completion_line(message: &str, outcome: ActivityOutcome, duration_ms: u64) -> String {
    let duration = format_duration(duration_ms);
    match outcome {
        ActivityOutcome::Succeeded => format!("{message}: done in {duration}"),
        ActivityOutcome::Failed => format!("{message}: failed after {duration}"),
    }
}

/// `850ms`, `12.3s` or `2m 05s`.
fn format_duration(duration_ms: u64) -> String {
    match duration_ms {
        0..1_000 => format!("{duration_ms}ms"),
        1_000..60_000 => format!("{}.{}s", duration_ms / 1_000, (duration_ms % 1_000) / 100),
        _ => {
            let seconds = duration_ms / 1_000;
            format!("{}m {:02}s", seconds / 60, seconds % 60)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_are_human_readable() {
        assert_eq!(format_duration(850), "850ms");
        assert_eq!(format_duration(12_345), "12.3s");
        assert_eq!(format_duration(125_000), "2m 05s");
    }

    #[test]
    fn completion_lines_name_the_outcome() {
        assert_eq!(
            completion_line(
                "Activating Nix flake runtime (.)",
                ActivityOutcome::Succeeded,
                12_345
            ),
            "Activating Nix flake runtime (.): done in 12.3s"
        );
        assert_eq!(
            completion_line("Downloading tools", ActivityOutcome::Failed, 850),
            "Downloading tools: failed after 850ms"
        );
    }
}
