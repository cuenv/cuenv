//! Terminal Progress Reporter
//!
//! Reports pipeline progress to the terminal with formatted output.
//! Uses tracing for output to integrate with the event system.

use async_trait::async_trait;
use std::sync::RwLock;

use super::{
    PipelineReport,
    progress::{LivePipelineProgress, LiveTaskProgress, LiveTaskStatus, ProgressReporter},
};

/// Terminal-based progress reporter.
///
/// Outputs task progress to the terminal via tracing macros.
/// Thread-safe for concurrent task execution.
pub struct TerminalReporter {
    /// Current pipeline progress state.
    progress: RwLock<Option<LivePipelineProgress>>,
    /// Whether to use verbose output.
    verbose: bool,
}

impl TerminalReporter {
    /// Create a new terminal reporter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            progress: RwLock::new(None),
            verbose: false,
        }
    }

    /// Create a verbose terminal reporter.
    #[must_use]
    pub fn verbose() -> Self {
        Self {
            progress: RwLock::new(None),
            verbose: true,
        }
    }

    /// Format a task status line.
    fn format_task_line(progress: &LiveTaskProgress) -> String {
        let icon = progress.status.icon();
        let duration = progress
            .duration
            .map(|d| format!(" ({:.2}s)", d.as_secs_f64()))
            .unwrap_or_default();

        format!("{} {}{}", icon, progress.name, duration)
    }
}

impl Default for TerminalReporter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProgressReporter for TerminalReporter {
    async fn pipeline_started(&self, name: &str, task_count: usize) {
        let progress = LivePipelineProgress::new(name, task_count);
        if let Ok(mut guard) = self.progress.write() {
            *guard = Some(progress);
        }

        tracing::info!(pipeline = name, tasks = task_count, "Starting CI pipeline");
    }

    async fn task_started(&self, task_id: &str, task_name: &str) {
        if let Ok(mut guard) = self.progress.write()
            && let Some(ref mut progress) = *guard
        {
            let task = LiveTaskProgress::pending(task_id, task_name).running();
            progress.tasks.push(task);
        }

        if self.verbose {
            tracing::info!(task = task_id, "Starting task: {}", task_name);
        }
    }

    async fn task_completed(&self, task_progress: &LiveTaskProgress) {
        if let Ok(mut guard) = self.progress.write()
            && let Some(ref mut progress) = *guard
        {
            progress.completed_tasks += 1;
            if task_progress.status == LiveTaskStatus::Cached {
                progress.cached_tasks += 1;
            }

            // Update the task in our list
            if let Some(task) = progress.tasks.iter_mut().find(|t| t.id == task_progress.id) {
                *task = task_progress.clone();
            }
        }

        let line = Self::format_task_line(task_progress);
        match task_progress.status {
            LiveTaskStatus::Success => {
                tracing::info!(task = %task_progress.id, "{}", line);
            }
            LiveTaskStatus::Failed => {
                if let Some(ref error) = task_progress.error {
                    tracing::error!(task = %task_progress.id, error = %error, "{}", line);
                } else {
                    tracing::error!(task = %task_progress.id, "{}", line);
                }
            }
            LiveTaskStatus::Cached => {
                tracing::info!(task = %task_progress.id, "{} (cached)", line);
            }
            _ => {
                tracing::info!(task = %task_progress.id, "{}", line);
            }
        }
    }

    async fn task_cached(&self, task_id: &str, task_name: &str) {
        if let Ok(mut guard) = self.progress.write()
            && let Some(ref mut progress) = *guard
        {
            progress.completed_tasks += 1;
            progress.cached_tasks += 1;

            let task = LiveTaskProgress::pending(task_id, task_name).cached();
            progress.tasks.push(task);
        }

        tracing::info!(
            task = task_id,
            "{} {} (cached)",
            LiveTaskStatus::Cached.icon(),
            task_name
        );
    }

    async fn task_progress(&self, task_id: &str, message: &str) {
        if self.verbose {
            tracing::debug!(task = task_id, "{}", message);
        }
    }

    #[allow(clippy::cast_precision_loss)] // u64 ms to f64 secs is fine for display
    async fn pipeline_completed(&self, report: &PipelineReport) {
        let total = report.tasks.len();
        let failed = report
            .tasks
            .iter()
            .filter(|t| t.status == super::TaskStatus::Failed)
            .count();
        let cached = report.cache_hits();
        let duration_secs = report.duration_ms.map_or(0.0, |ms| ms as f64 / 1000.0);

        if report.status == super::PipelineStatus::Success {
            tracing::info!(
                pipeline = %report.pipeline,
                total = total,
                cached = cached,
                duration = format!("{:.2}s", duration_secs),
                "Pipeline completed successfully"
            );
        } else {
            tracing::error!(
                pipeline = %report.pipeline,
                total = total,
                failed = failed,
                duration = format!("{:.2}s", duration_secs),
                "Pipeline failed"
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // unwrap is fine in tests
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_terminal_reporter_new() {
        let reporter = TerminalReporter::new();
        assert!(!reporter.verbose);
    }

    #[test]
    fn test_terminal_reporter_verbose() {
        let reporter = TerminalReporter::verbose();
        assert!(reporter.verbose);
    }

    #[test]
    fn test_terminal_reporter_default() {
        let reporter = TerminalReporter::default();
        assert!(!reporter.verbose);
    }

    #[test]
    fn test_format_task_line_success() {
        let progress = LiveTaskProgress::pending("build", "Build project")
            .completed(true, Duration::from_secs(5));
        let line = TerminalReporter::format_task_line(&progress);
        assert!(line.contains("Build project"));
        assert!(line.contains("5.00s"));
    }

    #[test]
    fn test_format_task_line_no_duration() {
        let progress = LiveTaskProgress::pending("build", "Build project");
        let line = TerminalReporter::format_task_line(&progress);
        assert!(line.contains("Build project"));
        assert!(!line.contains("s)")); // No duration
    }

    #[tokio::test]
    async fn test_terminal_reporter_pipeline_lifecycle() {
        let reporter = TerminalReporter::new();

        reporter.pipeline_started("test", 2).await;

        {
            let guard = reporter.progress.read().unwrap();
            let progress = guard.as_ref().unwrap();
            assert_eq!(progress.name, "test");
            assert_eq!(progress.total_tasks, 2);
        }

        reporter.task_started("t1", "Task 1").await;

        {
            let guard = reporter.progress.read().unwrap();
            let progress = guard.as_ref().unwrap();
            assert_eq!(progress.tasks.len(), 1);
            assert_eq!(progress.tasks[0].status, LiveTaskStatus::Running);
        }

        let task =
            LiveTaskProgress::pending("t1", "Task 1").completed(true, Duration::from_secs(1));
        reporter.task_completed(&task).await;

        {
            let guard = reporter.progress.read().unwrap();
            let progress = guard.as_ref().unwrap();
            assert_eq!(progress.completed_tasks, 1);
        }
    }

    #[tokio::test]
    async fn test_terminal_reporter_cached_task() {
        let reporter = TerminalReporter::new();

        reporter.pipeline_started("test", 1).await;
        reporter.task_cached("t1", "Task 1").await;

        {
            let guard = reporter.progress.read().unwrap();
            let progress = guard.as_ref().unwrap();
            assert_eq!(progress.completed_tasks, 1);
            assert_eq!(progress.cached_tasks, 1);
        }
    }
}
