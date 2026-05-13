//! Group-aware spinner state for the CLI renderer.
//!
//! [`SpinnerRenderer`] owns an [`indicatif::MultiProgress`] plus a registry
//! of per-task and per-group [`indicatif::ProgressBar`]s. The CLI renderer
//! delegates here when stdout is a TTY; in non-TTY mode the CLI renderer
//! falls back to the existing eprintln-based output so CI logs stay
//! grep-able.

#![allow(
    clippy::print_stderr,
    // indicatif template strings deliberately use `{name}` placeholder
    // syntax that looks like Rust format args but is consumed by indicatif.
    clippy::literal_string_with_formatting_args
)]

use std::collections::HashMap;

use indicatif::{HumanDuration, MultiProgress, ProgressBar, ProgressStyle};

use crate::event::{CacheSkipReason, SkipReason, Stream, TaskEvent};

/// Tail of the most recent stdout/stderr line shown under a task's spinner.
const TAIL_PREFIX_BYTES: usize = 80;

/// Group-aware progress renderer wrapping [`indicatif`].
#[derive(Debug)]
pub struct SpinnerRenderer {
    multi: MultiProgress,
    /// Per-task spinners keyed by full task name.
    tasks: HashMap<String, ProgressBar>,
    /// Per-group aggregate bars keyed by group prefix.
    groups: HashMap<String, GroupBar>,
    /// Whether to mirror the latest output line under each task spinner.
    show_output_tail: bool,
}

#[derive(Debug)]
struct GroupBar {
    bar: ProgressBar,
    total: u64,
    succeeded: u64,
    failed: u64,
    skipped: u64,
}

impl SpinnerRenderer {
    /// Create a new spinner renderer.
    #[must_use]
    pub fn new() -> Self {
        Self {
            multi: MultiProgress::new(),
            tasks: HashMap::new(),
            groups: HashMap::new(),
            show_output_tail: false,
        }
    }

    /// Enable / disable mirroring the latest output line under each task
    /// spinner. Off by default to keep terminals quiet.
    #[must_use]
    pub const fn with_output_tail(mut self, enabled: bool) -> Self {
        self.show_output_tail = enabled;
        self
    }

    /// Print a line above the active spinners without corrupting their
    /// frames.
    ///
    /// indicatif redraws on stderr by repositioning the cursor; writing
    /// to stdout/stderr concurrently from another path shreds the frame.
    /// This method routes the write through [`MultiProgress::println`],
    /// which suspends the draw, prints the line above the bars, and
    /// resumes — the indicatif contract for interleaved output.
    ///
    /// Errors are swallowed (`tracing::warn!`'d) because the renderer
    /// cannot surface I/O failures and we'd rather keep the spinner
    /// running than abort.
    pub fn print_above(&self, line: &str) {
        if let Err(err) = self.multi.println(line) {
            tracing::warn!(error = %err, "multi.println failed; output line dropped");
        }
    }

    /// Apply a [`TaskEvent`] to the spinner state.
    pub fn apply(&mut self, event: &TaskEvent) {
        match event {
            TaskEvent::GroupStarted {
                name,
                task_count,
                sequential,
                max_concurrency,
                ..
            } => self.start_group(name, *task_count, *sequential, *max_concurrency),
            TaskEvent::GroupCompleted {
                name,
                duration_ms,
                succeeded,
                failed,
                skipped,
                ..
            } => self.finish_group(name, *duration_ms, *succeeded, *failed, *skipped),
            TaskEvent::Started {
                name,
                command,
                parent_group,
                ..
            } => self.start_task(name, command, parent_group.as_deref()),
            TaskEvent::Queued {
                name,
                queue_position,
                parent_group,
                ..
            } => self.queue_task(name, *queue_position, parent_group.as_deref()),
            TaskEvent::CacheHit {
                name, parent_group, ..
            } => self.finish_cache_hit(name, parent_group.as_deref()),
            TaskEvent::CacheSkipped { name, reason, .. } => self.note_cache_skipped(name, reason),
            TaskEvent::CacheMiss { .. } => {
                // No spinner change — just transitions to running.
            }
            TaskEvent::Output {
                name,
                stream,
                content,
                ..
            } => self.note_output(name, *stream, content),
            TaskEvent::Skipped {
                name,
                reason,
                parent_group,
                ..
            } => self.finish_skipped(name, reason, parent_group.as_deref()),
            TaskEvent::Retrying {
                name,
                attempt,
                max_attempts,
                ..
            } => self.note_retry(name, *attempt, *max_attempts),
            TaskEvent::Completed {
                name,
                success,
                exit_code,
                duration_ms,
                parent_group,
                ..
            } => self.finish_task(
                name,
                *success,
                *exit_code,
                *duration_ms,
                parent_group.as_deref(),
            ),
        }
    }

    fn start_group(
        &mut self,
        name: &str,
        task_count: usize,
        sequential: bool,
        max_concurrency: Option<u32>,
    ) {
        let total = u64::try_from(task_count).unwrap_or(u64::MAX);
        let style = ProgressStyle::with_template(
            "{prefix:.bold.cyan} {bar:25.cyan/blue} {pos}/{len} {msg}",
        )
        .unwrap_or_else(|_| ProgressStyle::default_bar())
        .progress_chars("=> ");
        let bar = self.multi.add(ProgressBar::new(total));
        bar.set_style(style);
        bar.set_prefix(format!("[{name}]"));
        let mode = if sequential { "seq" } else { "par" };
        let limit = max_concurrency
            .map(|n| format!(" cap={n}"))
            .unwrap_or_default();
        bar.set_message(format!("{mode}{limit}"));
        self.groups.insert(
            name.to_string(),
            GroupBar {
                bar,
                total,
                succeeded: 0,
                failed: 0,
                skipped: 0,
            },
        );
    }

    fn finish_group(
        &mut self,
        name: &str,
        duration_ms: u64,
        succeeded: usize,
        failed: usize,
        skipped: usize,
    ) {
        let Some(group) = self.groups.remove(name) else {
            return;
        };
        let duration = HumanDuration(std::time::Duration::from_millis(duration_ms));
        group.bar.set_length(group.total);
        let ok = u64::try_from(succeeded).unwrap_or(u64::MAX);
        let bad = u64::try_from(failed).unwrap_or(u64::MAX);
        let skp = u64::try_from(skipped).unwrap_or(u64::MAX);
        group.bar.set_position(ok + bad + skp);
        group.bar.finish_with_message(format!(
            "{ok} ok, {bad} failed, {skp} skipped in {duration}"
        ));
    }

    fn start_task(&mut self, name: &str, command: &str, parent_group: Option<&str>) {
        let bar = self.spawn_task_bar(name, parent_group);
        bar.set_message(command.to_string());
        bar.enable_steady_tick(std::time::Duration::from_millis(120));
    }

    fn queue_task(&mut self, name: &str, queue_position: usize, parent_group: Option<&str>) {
        let bar = self.spawn_task_bar(name, parent_group);
        bar.set_message(format!("queued (#{queue_position})"));
    }

    fn spawn_task_bar(&mut self, name: &str, parent_group: Option<&str>) -> &mut ProgressBar {
        self.tasks.entry(name.to_string()).or_insert_with(|| {
            let style = ProgressStyle::with_template("{spinner:.green} {prefix:.dim} {wide_msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner());
            let bar = self.multi.add(ProgressBar::new_spinner());
            bar.set_style(style);
            bar.set_prefix(task_prefix(name, parent_group));
            bar
        })
    }

    fn note_output(&mut self, name: &str, stream: Stream, content: &str) {
        if !self.show_output_tail {
            return;
        }
        let Some(bar) = self.tasks.get(name) else {
            return;
        };
        let tag = match stream {
            Stream::Stdout => "out",
            Stream::Stderr => "err",
        };
        let trimmed: String = content.chars().take(TAIL_PREFIX_BYTES).collect();
        bar.set_message(format!("[{tag}] {trimmed}"));
    }

    fn note_cache_skipped(&mut self, name: &str, reason: &CacheSkipReason) {
        if let Some(bar) = self.tasks.get(name) {
            bar.set_message(format!("running (cache: {reason})"));
        }
    }

    fn note_retry(&mut self, name: &str, attempt: u32, max_attempts: u32) {
        if let Some(bar) = self.tasks.get(name) {
            bar.set_message(format!("retrying ({attempt}/{max_attempts})"));
        }
    }

    fn finish_cache_hit(&mut self, name: &str, parent_group: Option<&str>) {
        let bar = self
            .tasks
            .remove(name)
            .unwrap_or_else(|| self.fresh_finish_bar(name, parent_group));
        bar.finish_with_message("cached");
        self.tick_group(parent_group, GroupOutcome::Succeeded);
    }

    fn finish_skipped(&mut self, name: &str, reason: &SkipReason, parent_group: Option<&str>) {
        let bar = self
            .tasks
            .remove(name)
            .unwrap_or_else(|| self.fresh_finish_bar(name, parent_group));
        bar.finish_with_message(format!("skipped ({reason})"));
        self.tick_group(parent_group, GroupOutcome::Skipped);
    }

    fn finish_task(
        &mut self,
        name: &str,
        success: bool,
        exit_code: Option<i32>,
        duration_ms: u64,
        parent_group: Option<&str>,
    ) {
        let bar = self
            .tasks
            .remove(name)
            .unwrap_or_else(|| self.fresh_finish_bar(name, parent_group));
        let duration = HumanDuration(std::time::Duration::from_millis(duration_ms));
        if success {
            bar.finish_with_message(format!("ok in {duration}"));
            self.tick_group(parent_group, GroupOutcome::Succeeded);
        } else {
            let code = exit_code.map(|c| format!(" exit={c}")).unwrap_or_default();
            bar.finish_with_message(format!("failed{code} in {duration}"));
            self.tick_group(parent_group, GroupOutcome::Failed);
        }
    }

    fn fresh_finish_bar(&mut self, name: &str, parent_group: Option<&str>) -> ProgressBar {
        let style = ProgressStyle::with_template("{prefix} {wide_msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner());
        let bar = self.multi.add(ProgressBar::new_spinner());
        bar.set_style(style);
        bar.set_prefix(task_prefix(name, parent_group));
        bar
    }

    fn tick_group(&mut self, parent_group: Option<&str>, outcome: GroupOutcome) {
        let Some(group_name) = parent_group else {
            return;
        };
        let Some(group) = self.groups.get_mut(group_name) else {
            return;
        };
        match outcome {
            GroupOutcome::Succeeded => group.succeeded += 1,
            GroupOutcome::Failed => group.failed += 1,
            GroupOutcome::Skipped => group.skipped += 1,
        }
        let done = group.succeeded + group.failed + group.skipped;
        group.bar.set_position(done);
    }
}

impl Default for SpinnerRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
enum GroupOutcome {
    Succeeded,
    Failed,
    Skipped,
}

fn task_prefix(name: &str, parent_group: Option<&str>) -> String {
    match parent_group {
        Some(group) if name.starts_with(&format!("{group}.")) => {
            let short = &name[group.len() + 1..];
            format!("[{group}] {short}")
        }
        Some(group) => format!("[{group}] {name}"),
        None => format!("[{name}]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::TaskKind;

    fn started(name: &str, parent_group: Option<&str>) -> TaskEvent {
        TaskEvent::Started {
            name: name.to_string(),
            command: "cmd".to_string(),
            hermetic: false,
            parent_group: parent_group.map(str::to_string),
            task_kind: TaskKind::Task,
        }
    }

    fn completed(name: &str, parent_group: Option<&str>, success: bool) -> TaskEvent {
        TaskEvent::Completed {
            name: name.to_string(),
            success,
            exit_code: Some(i32::from(!success)),
            duration_ms: 12,
            parent_group: parent_group.map(str::to_string),
        }
    }

    #[test]
    fn task_prefix_strips_known_group() {
        assert_eq!(task_prefix("ci.fmt", Some("ci")), "[ci] fmt");
        assert_eq!(task_prefix("ci.fmt", None), "[ci.fmt]");
        // When the task name doesn't share the group prefix we fall back
        // to printing both — better than silently lying about the path.
        assert_eq!(task_prefix("other.fmt", Some("ci")), "[ci] other.fmt");
    }

    #[test]
    fn group_lifecycle_does_not_panic() {
        let mut spinner = SpinnerRenderer::new();
        spinner.apply(&TaskEvent::GroupStarted {
            name: "ci".to_string(),
            sequential: false,
            task_count: 2,
            parent_group: None,
            max_concurrency: Some(2),
        });
        spinner.apply(&started("ci.fmt", Some("ci")));
        spinner.apply(&completed("ci.fmt", Some("ci"), true));
        spinner.apply(&started("ci.test", Some("ci")));
        spinner.apply(&completed("ci.test", Some("ci"), false));
        spinner.apply(&TaskEvent::GroupCompleted {
            name: "ci".to_string(),
            success: false,
            duration_ms: 1000,
            parent_group: None,
            succeeded: 1,
            failed: 1,
            skipped: 0,
        });
    }

    #[test]
    fn cache_hit_finishes_task_bar() {
        let mut spinner = SpinnerRenderer::new();
        spinner.apply(&started("ci.lint", Some("ci")));
        spinner.apply(&TaskEvent::CacheHit {
            name: "ci.lint".to_string(),
            cache_key: "abc".to_string(),
            parent_group: Some("ci".to_string()),
        });
        assert!(!spinner.tasks.contains_key("ci.lint"));
    }

    #[test]
    fn print_above_does_not_panic_with_no_bars() {
        let spinner = SpinnerRenderer::new();
        // Just exercise the indicatif println path; in a non-TTY test
        // environment it may no-op, but it must not panic.
        spinner.print_above("hello above");
    }

    #[test]
    fn print_above_does_not_panic_with_active_bars() {
        let mut spinner = SpinnerRenderer::new();
        spinner.apply(&started("ci.fmt", None));
        spinner.print_above("output line while spinner active");
        spinner.apply(&completed("ci.fmt", None, true));
    }
}
