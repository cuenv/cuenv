//! Live Progress Reporter Trait
//!
//! Defines the interface for reporting pipeline execution progress in real-time.
//! Implementations can target terminals, GitHub Check Runs, or other backends.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use super::{PipelineReport, TaskStatus};

/// Status of a task during live execution (extends TaskStatus with Running state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LiveTaskStatus {
    /// Task is waiting for dependencies.
    Pending,
    /// Task is currently executing.
    Running,
    /// Task completed successfully.
    Success,
    /// Task failed.
    Failed,
    /// Task was restored from cache.
    Cached,
    /// Task was skipped.
    Skipped,
}

impl From<TaskStatus> for LiveTaskStatus {
    fn from(status: TaskStatus) -> Self {
        match status {
            TaskStatus::Success => Self::Success,
            TaskStatus::Failed => Self::Failed,
            TaskStatus::Cached => Self::Cached,
            TaskStatus::Skipped => Self::Skipped,
        }
    }
}

impl LiveTaskStatus {
    /// Get an icon representing this status.
    #[must_use]
    pub const fn icon(&self) -> &'static str {
        match self {
            Self::Pending => "\u{23f3}", // hourglass
            Self::Running => "\u{2699}", // gear
            Self::Success => "\u{2705}", // check mark
            Self::Failed => "\u{274c}",  // x
            Self::Cached => "\u{26a1}",  // lightning bolt
            Self::Skipped => "\u{23ed}", // skip forward
        }
    }

    /// Check if this is a terminal state.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Success | Self::Failed | Self::Cached | Self::Skipped
        )
    }
}

/// Live progress information for a single task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveTaskProgress {
    /// Task identifier.
    pub id: String,
    /// Human-readable task name.
    pub name: String,
    /// Current status.
    pub status: LiveTaskStatus,
    /// Execution duration (if started).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<Duration>,
    /// Error message (if failed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl LiveTaskProgress {
    /// Create a new pending task progress.
    #[must_use]
    pub fn pending(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            status: LiveTaskStatus::Pending,
            duration: None,
            error: None,
        }
    }

    /// Mark task as running.
    #[must_use]
    pub fn running(mut self) -> Self {
        self.status = LiveTaskStatus::Running;
        self
    }

    /// Mark task as completed with duration.
    #[must_use]
    pub fn completed(mut self, success: bool, duration: Duration) -> Self {
        self.status = if success {
            LiveTaskStatus::Success
        } else {
            LiveTaskStatus::Failed
        };
        self.duration = Some(duration);
        self
    }

    /// Mark task as cached.
    #[must_use]
    pub fn cached(mut self) -> Self {
        self.status = LiveTaskStatus::Cached;
        self
    }

    /// Mark task as failed with error message.
    #[must_use]
    pub fn failed(mut self, error: impl Into<String>, duration: Duration) -> Self {
        self.status = LiveTaskStatus::Failed;
        self.duration = Some(duration);
        self.error = Some(error.into());
        self
    }
}

/// Live progress of a pipeline execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LivePipelineProgress {
    /// Pipeline name.
    pub name: String,
    /// Total number of tasks.
    pub total_tasks: usize,
    /// Number of completed tasks (success or failure).
    pub completed_tasks: usize,
    /// Number of cached tasks.
    pub cached_tasks: usize,
    /// Current task statuses.
    pub tasks: Vec<LiveTaskProgress>,
}

impl LivePipelineProgress {
    /// Create a new pipeline progress tracker.
    #[must_use]
    pub fn new(name: impl Into<String>, task_count: usize) -> Self {
        Self {
            name: name.into(),
            total_tasks: task_count,
            completed_tasks: 0,
            cached_tasks: 0,
            tasks: Vec::with_capacity(task_count),
        }
    }

    /// Calculate completion percentage.
    #[must_use]
    pub fn percentage(&self) -> f32 {
        if self.total_tasks == 0 {
            100.0
        } else {
            #[allow(clippy::cast_precision_loss)]
            let completed = self.completed_tasks as f32;
            #[allow(clippy::cast_precision_loss)]
            let total = self.total_tasks as f32;
            (completed / total) * 100.0
        }
    }
}

/// Trait for reporting pipeline execution progress in real-time.
///
/// Implementations can target different output backends:
/// - Terminal (progress bars, spinners)
/// - GitHub Check Runs (live status updates)
/// - JSON output (for CI integration)
#[async_trait]
pub trait ProgressReporter: Send + Sync {
    /// Called when a pipeline starts execution.
    async fn pipeline_started(&self, name: &str, task_count: usize);

    /// Called when a task starts executing.
    async fn task_started(&self, task_id: &str, task_name: &str);

    /// Called when a task completes (success or failure).
    async fn task_completed(&self, progress: &LiveTaskProgress);

    /// Called when a task is restored from cache.
    async fn task_cached(&self, task_id: &str, task_name: &str);

    /// Called periodically for long-running tasks.
    async fn task_progress(&self, task_id: &str, message: &str);

    /// Called when the pipeline completes.
    async fn pipeline_completed(&self, report: &PipelineReport);
}

/// No-op reporter for when progress reporting is disabled.
#[derive(Debug, Default)]
pub struct NoOpReporter;

#[async_trait]
impl ProgressReporter for NoOpReporter {
    async fn pipeline_started(&self, _name: &str, _task_count: usize) {}
    async fn task_started(&self, _task_id: &str, _task_name: &str) {}
    async fn task_completed(&self, _progress: &LiveTaskProgress) {}
    async fn task_cached(&self, _task_id: &str, _task_name: &str) {}
    async fn task_progress(&self, _task_id: &str, _message: &str) {}
    async fn pipeline_completed(&self, _report: &PipelineReport) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_live_task_status_icon() {
        assert_eq!(LiveTaskStatus::Pending.icon(), "\u{23f3}");
        assert_eq!(LiveTaskStatus::Running.icon(), "\u{2699}");
        assert_eq!(LiveTaskStatus::Success.icon(), "\u{2705}");
        assert_eq!(LiveTaskStatus::Failed.icon(), "\u{274c}");
        assert_eq!(LiveTaskStatus::Cached.icon(), "\u{26a1}");
        assert_eq!(LiveTaskStatus::Skipped.icon(), "\u{23ed}");
    }

    #[test]
    fn test_live_task_status_is_terminal() {
        assert!(!LiveTaskStatus::Pending.is_terminal());
        assert!(!LiveTaskStatus::Running.is_terminal());
        assert!(LiveTaskStatus::Success.is_terminal());
        assert!(LiveTaskStatus::Failed.is_terminal());
        assert!(LiveTaskStatus::Cached.is_terminal());
        assert!(LiveTaskStatus::Skipped.is_terminal());
    }

    #[test]
    fn test_live_task_status_from_task_status() {
        assert_eq!(
            LiveTaskStatus::from(TaskStatus::Success),
            LiveTaskStatus::Success
        );
        assert_eq!(
            LiveTaskStatus::from(TaskStatus::Failed),
            LiveTaskStatus::Failed
        );
        assert_eq!(
            LiveTaskStatus::from(TaskStatus::Cached),
            LiveTaskStatus::Cached
        );
        assert_eq!(
            LiveTaskStatus::from(TaskStatus::Skipped),
            LiveTaskStatus::Skipped
        );
    }

    #[test]
    fn test_live_task_progress_pending() {
        let progress = LiveTaskProgress::pending("build", "Build project");
        assert_eq!(progress.id, "build");
        assert_eq!(progress.name, "Build project");
        assert_eq!(progress.status, LiveTaskStatus::Pending);
        assert!(progress.duration.is_none());
        assert!(progress.error.is_none());
    }

    #[test]
    fn test_live_task_progress_running() {
        let progress = LiveTaskProgress::pending("build", "Build project").running();
        assert_eq!(progress.status, LiveTaskStatus::Running);
    }

    #[test]
    fn test_live_task_progress_completed_success() {
        let progress = LiveTaskProgress::pending("build", "Build project")
            .completed(true, Duration::from_secs(5));
        assert_eq!(progress.status, LiveTaskStatus::Success);
        assert_eq!(progress.duration, Some(Duration::from_secs(5)));
    }

    #[test]
    fn test_live_task_progress_completed_failure() {
        let progress = LiveTaskProgress::pending("build", "Build project")
            .completed(false, Duration::from_secs(3));
        assert_eq!(progress.status, LiveTaskStatus::Failed);
    }

    #[test]
    fn test_live_task_progress_cached() {
        let progress = LiveTaskProgress::pending("build", "Build project").cached();
        assert_eq!(progress.status, LiveTaskStatus::Cached);
    }

    #[test]
    fn test_live_task_progress_failed_with_error() {
        let progress = LiveTaskProgress::pending("build", "Build project")
            .failed("Compilation error", Duration::from_secs(2));
        assert_eq!(progress.status, LiveTaskStatus::Failed);
        assert_eq!(progress.error, Some("Compilation error".to_string()));
        assert_eq!(progress.duration, Some(Duration::from_secs(2)));
    }

    #[test]
    fn test_live_pipeline_progress_new() {
        let progress = LivePipelineProgress::new("default", 10);
        assert_eq!(progress.name, "default");
        assert_eq!(progress.total_tasks, 10);
        assert_eq!(progress.completed_tasks, 0);
        assert_eq!(progress.cached_tasks, 0);
        assert!(progress.tasks.is_empty());
    }

    #[test]
    fn test_live_pipeline_progress_percentage() {
        let mut progress = LivePipelineProgress::new("default", 10);
        assert_eq!(progress.percentage(), 0.0);

        progress.completed_tasks = 5;
        assert_eq!(progress.percentage(), 50.0);

        progress.completed_tasks = 10;
        assert_eq!(progress.percentage(), 100.0);
    }

    #[test]
    fn test_live_pipeline_progress_percentage_empty() {
        let progress = LivePipelineProgress::new("default", 0);
        assert_eq!(progress.percentage(), 100.0);
    }

    #[tokio::test]
    async fn test_noop_reporter() {
        let reporter = NoOpReporter;

        // All methods should succeed without error
        reporter.pipeline_started("test", 5).await;
        reporter.task_started("t1", "Task 1").await;
        reporter.task_cached("t1", "Task 1").await;
        reporter.task_progress("t1", "Working...").await;

        let progress =
            LiveTaskProgress::pending("t1", "Task 1").completed(true, Duration::from_secs(1));
        reporter.task_completed(&progress).await;
    }
}
