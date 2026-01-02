//! GitHub Check Run Progress Reporter
//!
//! Reports pipeline progress to GitHub Check Runs with live updates.
//! Shows task-by-task progress in the Check Run summary.
//!
//! This module provides the reporter abstraction and summary generation.
//! The actual GitHub API calls are delegated to a `GitHubApiClient` trait
//! which can be implemented by the cuenv-github crate.

// RwLock::unwrap() is safe - only panics if poisoned (which indicates a bug)
// format_push_string is fine for clarity in summary generation
#![allow(clippy::unwrap_used, clippy::format_push_string)]

use async_trait::async_trait;
use std::sync::{Arc, RwLock};

use super::{
    PipelineReport, PipelineStatus, TaskStatus,
    progress::{LivePipelineProgress, LiveTaskProgress, LiveTaskStatus, ProgressReporter},
};

/// Trait for GitHub API operations.
///
/// This trait abstracts the GitHub API calls so the reporter can work
/// without a direct octocrab dependency. Implementations can be provided
/// by cuenv-github or mock implementations for testing.
#[async_trait]
pub trait GitHubApiClient: Send + Sync {
    /// Create a new check run and return its ID.
    async fn create_check_run(&self, name: &str, sha: &str) -> Result<u64, String>;

    /// Update a check run with a new summary.
    async fn update_check_run(&self, check_run_id: u64, summary: &str) -> Result<(), String>;

    /// Complete a check run with conclusion and final output.
    async fn complete_check_run(
        &self,
        check_run_id: u64,
        success: bool,
        summary: &str,
        annotations: Vec<CheckAnnotation>,
    ) -> Result<(), String>;
}

/// Annotation for a check run failure.
#[derive(Debug, Clone)]
pub struct CheckAnnotation {
    /// File path for the annotation.
    pub path: String,
    /// Line number.
    pub line: u32,
    /// Annotation message.
    pub message: String,
    /// Annotation title.
    pub title: Option<String>,
}

/// Configuration for the GitHub Check Run reporter.
#[derive(Debug, Clone)]
pub struct GitHubReporterConfig {
    /// Git commit SHA for the check run.
    pub sha: String,
    /// Check run name.
    pub check_name: String,
    /// Minimum interval between updates (to avoid rate limits).
    pub update_interval_ms: u64,
}

impl Default for GitHubReporterConfig {
    fn default() -> Self {
        Self {
            sha: String::new(),
            check_name: "cuenv CI".to_string(),
            update_interval_ms: 3000,
        }
    }
}

impl GitHubReporterConfig {
    /// Create a new GitHub reporter config.
    #[must_use]
    pub fn new(sha: impl Into<String>) -> Self {
        Self {
            sha: sha.into(),
            ..Default::default()
        }
    }

    /// Set the check run name.
    #[must_use]
    pub fn with_check_name(mut self, name: impl Into<String>) -> Self {
        self.check_name = name.into();
        self
    }

    /// Set the minimum update interval.
    #[must_use]
    pub const fn with_update_interval_ms(mut self, ms: u64) -> Self {
        self.update_interval_ms = ms;
        self
    }
}

/// GitHub Check Run progress reporter.
///
/// Reports pipeline progress to GitHub Check Runs with:
/// - Live task-by-task status updates
/// - Progress percentage in the summary
/// - Final annotations for failures
pub struct GitHubCheckReporter<C: GitHubApiClient> {
    /// GitHub API client.
    client: Arc<C>,
    /// Configuration.
    config: GitHubReporterConfig,
    /// Check Run ID (set after creation).
    check_run_id: RwLock<Option<u64>>,
    /// Current pipeline progress.
    progress: RwLock<Option<LivePipelineProgress>>,
    /// Last update time (for rate limiting).
    last_update: RwLock<std::time::Instant>,
}

impl<C: GitHubApiClient> GitHubCheckReporter<C> {
    /// Create a new GitHub Check Run reporter.
    #[must_use]
    pub fn new(client: Arc<C>, config: GitHubReporterConfig) -> Self {
        Self {
            client,
            config,
            check_run_id: RwLock::new(None),
            progress: RwLock::new(None),
            last_update: RwLock::new(std::time::Instant::now()),
        }
    }

    /// Generate a summary table for the current progress.
    pub fn generate_summary(&self) -> String {
        let guard = self.progress.read().unwrap();
        let Some(progress) = guard.as_ref() else {
            return "Pipeline starting...".to_string();
        };

        let mut summary = String::new();

        // Header with progress bar
        let percentage = progress.percentage();
        let bar = generate_progress_bar(percentage);
        summary.push_str(&format!("## {} Pipeline\n\n", progress.name));
        summary.push_str(&format!(
            "{} {:.0}% complete ({}/{} tasks)\n\n",
            bar, percentage, progress.completed_tasks, progress.total_tasks
        ));

        // Task table
        if !progress.tasks.is_empty() {
            summary.push_str("| Task | Status | Duration |\n");
            summary.push_str("|------|--------|----------|\n");

            for task in &progress.tasks {
                let icon = task.status.icon();
                let duration = task
                    .duration
                    .map_or_else(|| "-".to_string(), |d| format!("{:.2}s", d.as_secs_f64()));

                let status_text = match task.status {
                    LiveTaskStatus::Pending => "Pending",
                    LiveTaskStatus::Running => "Running",
                    LiveTaskStatus::Success => "Success",
                    LiveTaskStatus::Failed => "Failed",
                    LiveTaskStatus::Cached => "Cached",
                    LiveTaskStatus::Skipped => "Skipped",
                };

                summary.push_str(&format!(
                    "| {} | {} {} | {} |\n",
                    task.name, icon, status_text, duration
                ));
            }
        }

        summary
    }

    /// Update the check run with current progress (if rate limit allows).
    async fn maybe_update_check(&self) {
        // Check rate limit
        let elapsed = {
            let last = self.last_update.read().unwrap();
            last.elapsed()
        };

        if elapsed.as_millis() < u128::from(self.config.update_interval_ms) {
            return;
        }

        // Update last update time
        {
            let mut last = self.last_update.write().unwrap();
            *last = std::time::Instant::now();
        }

        // Get check run ID
        let check_run_id = {
            let guard = self.check_run_id.read().unwrap();
            *guard
        };

        let Some(id) = check_run_id else {
            tracing::debug!("No check run ID, skipping update");
            return;
        };

        let summary = self.generate_summary();

        // Update via GitHub API
        if let Err(e) = self.client.update_check_run(id, &summary).await {
            tracing::warn!(error = %e, "Failed to update check run");
        }
    }

    /// Force an update to the check run.
    async fn force_update_check(&self) {
        // Update last update time
        {
            let mut last = self.last_update.write().unwrap();
            *last = std::time::Instant::now();
        }

        // Get check run ID
        let check_run_id = {
            let guard = self.check_run_id.read().unwrap();
            *guard
        };

        let Some(id) = check_run_id else {
            tracing::debug!("No check run ID, skipping update");
            return;
        };

        let summary = self.generate_summary();

        // Update via GitHub API
        if let Err(e) = self.client.update_check_run(id, &summary).await {
            tracing::warn!(error = %e, "Failed to update check run");
        }
    }
}

#[async_trait]
impl<C: GitHubApiClient + 'static> ProgressReporter for GitHubCheckReporter<C> {
    async fn pipeline_started(&self, name: &str, task_count: usize) {
        let progress = LivePipelineProgress::new(name, task_count);
        *self.progress.write().unwrap() = Some(progress);

        // Create the check run
        match self
            .client
            .create_check_run(&self.config.check_name, &self.config.sha)
            .await
        {
            Ok(id) => {
                *self.check_run_id.write().unwrap() = Some(id);
                tracing::info!(check_run_id = id, "Created GitHub check run");
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to create GitHub check run");
            }
        }
    }

    async fn task_started(&self, task_id: &str, task_name: &str) {
        if let Ok(mut guard) = self.progress.write() {
            if let Some(ref mut progress) = *guard {
                let task = LiveTaskProgress::pending(task_id, task_name).running();
                progress.tasks.push(task);
            }
        }

        self.maybe_update_check().await;
    }

    async fn task_completed(&self, task_progress: &LiveTaskProgress) {
        if let Ok(mut guard) = self.progress.write() {
            if let Some(ref mut progress) = *guard {
                progress.completed_tasks += 1;
                if task_progress.status == LiveTaskStatus::Cached {
                    progress.cached_tasks += 1;
                }

                // Update the task in our list
                if let Some(task) = progress.tasks.iter_mut().find(|t| t.id == task_progress.id) {
                    *task = task_progress.clone();
                }
            }
        }

        // Force update on completion
        self.force_update_check().await;
    }

    async fn task_cached(&self, task_id: &str, task_name: &str) {
        if let Ok(mut guard) = self.progress.write() {
            if let Some(ref mut progress) = *guard {
                progress.completed_tasks += 1;
                progress.cached_tasks += 1;

                let task = LiveTaskProgress::pending(task_id, task_name).cached();
                progress.tasks.push(task);
            }
        }

        self.maybe_update_check().await;
    }

    async fn task_progress(&self, _task_id: &str, _message: &str) {
        self.maybe_update_check().await;
    }

    async fn pipeline_completed(&self, report: &PipelineReport) {
        // Get check run ID
        let check_run_id = {
            let guard = self.check_run_id.read().unwrap();
            *guard
        };

        let Some(id) = check_run_id else {
            tracing::warn!("No check run ID, cannot complete");
            return;
        };

        // Build annotations for failures
        let annotations: Vec<CheckAnnotation> = report
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Failed)
            .map(|t| CheckAnnotation {
                path: "env.cue".to_string(),
                line: 1,
                message: format!("Task '{}' failed", t.name),
                title: Some(format!("Task {} failed", t.name)),
            })
            .collect();

        let summary = generate_final_summary(report);
        let success = report.status == PipelineStatus::Success;

        if let Err(e) = self
            .client
            .complete_check_run(id, success, &summary, annotations)
            .await
        {
            tracing::warn!(error = %e, "Failed to complete GitHub check run");
        }
    }
}

/// Generate a text-based progress bar.
fn generate_progress_bar(percentage: f32) -> String {
    let filled = (percentage / 10.0).round() as usize;
    let empty = 10 - filled;

    format!(
        "[{}{}]",
        "\u{2588}".repeat(filled),
        "\u{2591}".repeat(empty)
    )
}

/// Generate the final summary for a completed pipeline.
fn generate_final_summary(report: &PipelineReport) -> String {
    let mut summary = String::new();

    let status_emoji = match report.status {
        PipelineStatus::Success => "\u{2705}",
        PipelineStatus::Failed => "\u{274c}",
        PipelineStatus::Partial => "\u{26a0}",
        PipelineStatus::Pending => "\u{23f3}",
    };

    summary.push_str(&format!(
        "## {} Pipeline: {}\n\n",
        status_emoji,
        match report.status {
            PipelineStatus::Success => "Passed",
            PipelineStatus::Failed => "Failed",
            PipelineStatus::Partial => "Partial",
            PipelineStatus::Pending => "Pending",
        }
    ));

    // Duration
    if let Some(duration_ms) = report.duration_ms {
        #[allow(clippy::cast_precision_loss)]
        let duration_secs = duration_ms as f64 / 1000.0;
        summary.push_str(&format!("**Duration:** {duration_secs:.2}s\n\n"));
    }

    // Task summary
    let total = report.tasks.len();
    let succeeded = report
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Success)
        .count();
    let cached = report
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Cached)
        .count();
    let failed = report
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Failed)
        .count();

    summary.push_str(&format!(
        "**Tasks:** {total} total, {succeeded} succeeded, {cached} cached, {failed} failed\n\n"
    ));

    // Task table
    if !report.tasks.is_empty() {
        summary.push_str("| Task | Status | Duration |\n");
        summary.push_str("|------|--------|----------|\n");

        for task in &report.tasks {
            let icon = match task.status {
                TaskStatus::Success => "\u{2705}",
                TaskStatus::Failed => "\u{274c}",
                TaskStatus::Cached => "\u{26a1}",
                TaskStatus::Skipped => "\u{23ed}",
            };

            #[allow(clippy::cast_precision_loss)]
            let duration = format!("{:.2}s", task.duration_ms as f64 / 1000.0);

            summary.push_str(&format!(
                "| {} | {} {:?} | {} |\n",
                task.name, icon, task.status, duration
            ));
        }
    }

    summary
}

/// No-op GitHub API client for testing.
#[derive(Debug, Default)]
pub struct NoOpGitHubClient;

#[async_trait]
impl GitHubApiClient for NoOpGitHubClient {
    async fn create_check_run(&self, _name: &str, _sha: &str) -> Result<u64, String> {
        Ok(0)
    }

    async fn update_check_run(&self, _check_run_id: u64, _summary: &str) -> Result<(), String> {
        Ok(())
    }

    async fn complete_check_run(
        &self,
        _check_run_id: u64,
        _success: bool,
        _summary: &str,
        _annotations: Vec<CheckAnnotation>,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_generate_progress_bar() {
        assert_eq!(
            generate_progress_bar(0.0),
            "[\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}]"
        );
        assert_eq!(
            generate_progress_bar(50.0),
            "[\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}]"
        );
        assert_eq!(
            generate_progress_bar(100.0),
            "[\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}]"
        );
    }

    #[test]
    fn test_github_reporter_config() {
        let config = GitHubReporterConfig::new("sha123")
            .with_check_name("My Check")
            .with_update_interval_ms(5000);

        assert_eq!(config.sha, "sha123");
        assert_eq!(config.check_name, "My Check");
        assert_eq!(config.update_interval_ms, 5000);
    }

    #[test]
    fn test_github_reporter_creation() {
        let client = Arc::new(NoOpGitHubClient);
        let config = GitHubReporterConfig::new("sha123");
        let _reporter = GitHubCheckReporter::new(client, config);
    }

    #[test]
    fn test_generate_summary_empty() {
        let client = Arc::new(NoOpGitHubClient);
        let config = GitHubReporterConfig::new("sha123");
        let reporter = GitHubCheckReporter::new(client, config);

        let summary = reporter.generate_summary();
        assert_eq!(summary, "Pipeline starting...");
    }

    #[tokio::test]
    async fn test_github_reporter_pipeline_lifecycle() {
        let client = Arc::new(NoOpGitHubClient);
        let config = GitHubReporterConfig::new("sha123");
        let reporter = GitHubCheckReporter::new(client, config);

        // Start pipeline
        reporter.pipeline_started("test", 2).await;

        {
            let guard = reporter.progress.read().unwrap();
            let progress = guard.as_ref().unwrap();
            assert_eq!(progress.name, "test");
            assert_eq!(progress.total_tasks, 2);
        }

        // Start task
        reporter.task_started("t1", "Task 1").await;

        {
            let guard = reporter.progress.read().unwrap();
            let progress = guard.as_ref().unwrap();
            assert_eq!(progress.tasks.len(), 1);
            assert_eq!(progress.tasks[0].status, LiveTaskStatus::Running);
        }

        // Complete task
        let task =
            LiveTaskProgress::pending("t1", "Task 1").completed(true, Duration::from_secs(1));
        reporter.task_completed(&task).await;

        {
            let guard = reporter.progress.read().unwrap();
            let progress = guard.as_ref().unwrap();
            assert_eq!(progress.completed_tasks, 1);
        }
    }

    #[test]
    fn test_check_annotation() {
        let annotation = CheckAnnotation {
            path: "env.cue".to_string(),
            line: 10,
            message: "Task failed".to_string(),
            title: Some("Error".to_string()),
        };

        assert_eq!(annotation.path, "env.cue");
        assert_eq!(annotation.line, 10);
    }
}
