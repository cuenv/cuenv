use super::CliRenderer;
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
                // Bare println! / eprintln! here would race indicatif's
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
                eprintln!("> [{name}] {command}{hermetic_indicator}");
            }
            TaskEvent::CacheHit { name, .. } => {
                eprintln!("> [{name}] (cached)");
            }
            TaskEvent::CacheMiss { name, .. } => {
                if self.config.verbose {
                    eprintln!("> [{name}] cache miss, executing...");
                }
            }
            TaskEvent::CacheSkipped { name, reason, .. } => {
                if self.config.verbose {
                    eprintln!("> [{name}] cache skipped: {reason}");
                }
            }
            TaskEvent::Queued {
                name,
                queue_position,
                ..
            } => {
                if self.config.verbose {
                    eprintln!("> [{name}] queued (position {queue_position})");
                }
            }
            TaskEvent::Skipped { name, reason, .. } => {
                eprintln!("> [{name}] skipped ({reason})");
            }
            TaskEvent::Retrying {
                name,
                attempt,
                max_attempts,
                ..
            } => {
                eprintln!("> [{name}] retrying (attempt {attempt}/{max_attempts})");
            }
            TaskEvent::Output {
                stream, content, ..
            } => match stream {
                Stream::Stdout => {
                    println!("{content}");
                }
                Stream::Stderr => {
                    eprintln!("{content}");
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
                    eprintln!("> [{name}] {status} in {duration_ms}ms");
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
                eprintln!("> Running {mode} group: {name} ({task_count} tasks)");
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
                    eprintln!(
                        "> Group {name} {status} in {duration_ms}ms ({succeeded} ok, {failed} failed, {skipped} skipped)"
                    );
                }
            }
        }
    }
}
