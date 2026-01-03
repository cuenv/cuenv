//! CI Pipeline Reporting
//!
//! Provides types and traits for pipeline execution reporting, including:
//! - Static report types for completed pipelines
//! - Live progress reporting traits
//! - Terminal reporter (provider-specific reporters live in their own crates)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod json;
pub mod markdown;
pub mod progress;
pub mod terminal;

// Re-export commonly used types
pub use progress::{
    LivePipelineProgress, LiveTaskProgress, LiveTaskStatus, NoOpReporter, ProgressReporter,
};
pub use terminal::TerminalReporter;

/// Final pipeline report for a completed execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineReport {
    /// Report format version.
    pub version: String,
    /// Project name.
    pub project: String,
    /// Pipeline name.
    pub pipeline: String,
    /// Execution context (CI provider, event, etc.).
    pub context: ContextReport,
    /// When the pipeline started.
    pub started_at: DateTime<Utc>,
    /// When the pipeline completed.
    pub completed_at: Option<DateTime<Utc>>,
    /// Total duration in milliseconds.
    pub duration_ms: Option<u64>,
    /// Overall pipeline status.
    pub status: PipelineStatus,
    /// Individual task reports.
    pub tasks: Vec<TaskReport>,
}

impl PipelineReport {
    /// Get the number of cache hits.
    #[must_use]
    pub fn cache_hits(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Cached)
            .count()
    }
}

/// CI execution context information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextReport {
    /// CI provider name (github, buildkite, etc.).
    pub provider: String,
    /// Event type (push, pull_request, etc.).
    pub event: String,
    /// Current ref name (branch or tag).
    pub ref_name: String,
    /// Base ref for comparison (for PRs).
    pub base_ref: Option<String>,
    /// Git commit SHA.
    pub sha: String,
    /// List of changed files.
    pub changed_files: Vec<String>,
}

/// Individual task execution report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskReport {
    /// Task name.
    pub name: String,
    /// Task completion status.
    pub status: TaskStatus,
    /// Execution duration in milliseconds.
    pub duration_ms: u64,
    /// Process exit code (if applicable).
    pub exit_code: Option<i32>,
    /// Input files that matched.
    pub inputs_matched: Vec<String>,
    /// Cache key used.
    pub cache_key: Option<String>,
    /// Output files produced.
    pub outputs: Vec<String>,
}

/// Task completion status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    /// Task completed successfully.
    Success,
    /// Task failed.
    Failed,
    /// Task was restored from cache.
    Cached,
    /// Task was skipped.
    Skipped,
}

/// Overall pipeline status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PipelineStatus {
    /// Pipeline is pending execution.
    Pending,
    /// Pipeline completed successfully.
    Success,
    /// Pipeline failed.
    Failed,
    /// Pipeline partially completed.
    Partial,
}

/// Opaque handle for a check run (provider specific).
#[derive(Debug, Clone)]
pub struct CheckHandle {
    /// Provider-specific identifier.
    pub id: String,
}
