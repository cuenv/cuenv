use super::{CliRenderer, stderr_line, stdout_line};
use crate::event::{Stream, TaskEvent};

impl CliRenderer {
    pub(super) fn render_task(&self, event: &TaskEvent) {
        #[cfg(feature = "spinner")]
        if self.render_task_with_spinner(event) {
            return;
        }

        self.render_task_plain(event);
    }

    #[cfg(feature = "spinner")]
    fn render_task_with_spinner(&self, event: &TaskEvent) -> bool {
        if let Some(spinner) = self.spinner.as_ref() {
            // Best-effort lock: if a renderer panicked while holding it, keep
            // surfacing the event through the fallback path below.
            if let Ok(mut guard) = spinner.lock() {
                guard.apply(event);
                // Route output through MultiProgress::println so it appears
                // above the active spinners without corrupting their frames.
                // Direct terminal writes here would race indicatif's
                // cursor moves on stderr.
                if let TaskEvent::Output {
                    name,
                    stream,
                    content,
                    ..
                } = event
                {
                    let prefix = match stream {
                        Stream::Stdout => "  ",
                        Stream::Stderr => "! ",
                    };
                    guard.print_above(&format!("{prefix}[{name}] {content}"));
                }
                return true;
            }
        }

        false
    }

    fn render_task_plain(&self, event: &TaskEvent) {
        match event {
            TaskEvent::Started {
                name,
                command,
                hermetic,
                ..
            } => {
                let hermetic_indicator = if *hermetic { " (hermetic)" } else { "" };
                stderr_line(format_args!("> [{name}] {command}{hermetic_indicator}"));
            }
            TaskEvent::CacheHit { name, .. } => {
                stderr_line(format_args!("> [{name}] (cached)"));
            }
            TaskEvent::CacheMiss { name, .. } => {
                if self.config.verbose {
                    stderr_line(format_args!("> [{name}] cache miss, executing..."));
                }
            }
            TaskEvent::CacheSkipped { name, reason, .. } => {
                if self.config.verbose {
                    stderr_line(format_args!("> [{name}] cache skipped: {reason}"));
                }
            }
            TaskEvent::Queued {
                name,
                queue_position,
                ..
            } => {
                if self.config.verbose {
                    stderr_line(format_args!(
                        "> [{name}] queued (position {queue_position})"
                    ));
                }
            }
            TaskEvent::Skipped { name, reason, .. } => {
                stderr_line(format_args!("> [{name}] skipped ({reason})"));
            }
            TaskEvent::Retrying {
                name,
                attempt,
                max_attempts,
                ..
            } => {
                stderr_line(format_args!(
                    "> [{name}] retrying (attempt {attempt}/{max_attempts})"
                ));
            }
            TaskEvent::Output {
                stream, content, ..
            } => match stream {
                Stream::Stdout => {
                    stdout_line(format_args!("{content}"));
                }
                Stream::Stderr => {
                    stderr_line(format_args!("{content}"));
                }
            },
            TaskEvent::Completed {
                name,
                success,
                duration_ms,
                ..
            } => {
                if self.config.verbose {
                    let status = if *success { "completed" } else { "failed" };
                    stderr_line(format_args!("> [{name}] {status} in {duration_ms}ms"));
                }
            }
            TaskEvent::GroupStarted {
                name,
                sequential,
                task_count,
                ..
            } => {
                let mode = if *sequential {
                    "sequential"
                } else {
                    "parallel"
                };
                stderr_line(format_args!(
                    "> Running {mode} group: {name} ({task_count} tasks)"
                ));
            }
            TaskEvent::GroupCompleted {
                name,
                success,
                duration_ms,
                succeeded,
                failed,
                skipped,
                ..
            } => {
                if self.config.verbose {
                    let status = if *success { "completed" } else { "failed" };
                    stderr_line(format_args!(
                        "> Group {name} {status} in {duration_ms}ms ({succeeded} ok, {failed} failed, {skipped} skipped)"
                    ));
                }
            }
        }
    }
}
