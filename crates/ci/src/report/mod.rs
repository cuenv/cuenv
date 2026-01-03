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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_task_status_serde() {
        assert_eq!(
            serde_json::to_string(&TaskStatus::Success).unwrap(),
            r#""success""#
        );
        assert_eq!(
            serde_json::to_string(&TaskStatus::Failed).unwrap(),
            r#""failed""#
        );
        assert_eq!(
            serde_json::to_string(&TaskStatus::Cached).unwrap(),
            r#""cached""#
        );
        assert_eq!(
            serde_json::to_string(&TaskStatus::Skipped).unwrap(),
            r#""skipped""#
        );
    }

    #[test]
    fn test_task_status_deserialize() {
        assert_eq!(
            serde_json::from_str::<TaskStatus>(r#""success""#).unwrap(),
            TaskStatus::Success
        );
        assert_eq!(
            serde_json::from_str::<TaskStatus>(r#""failed""#).unwrap(),
            TaskStatus::Failed
        );
    }

    #[test]
    fn test_pipeline_status_serde() {
        assert_eq!(
            serde_json::to_string(&PipelineStatus::Pending).unwrap(),
            r#""pending""#
        );
        assert_eq!(
            serde_json::to_string(&PipelineStatus::Success).unwrap(),
            r#""success""#
        );
        assert_eq!(
            serde_json::to_string(&PipelineStatus::Failed).unwrap(),
            r#""failed""#
        );
        assert_eq!(
            serde_json::to_string(&PipelineStatus::Partial).unwrap(),
            r#""partial""#
        );
    }

    #[test]
    fn test_pipeline_status_deserialize() {
        assert_eq!(
            serde_json::from_str::<PipelineStatus>(r#""pending""#).unwrap(),
            PipelineStatus::Pending
        );
        assert_eq!(
            serde_json::from_str::<PipelineStatus>(r#""partial""#).unwrap(),
            PipelineStatus::Partial
        );
    }

    #[test]
    fn test_task_report_serde() {
        let report = TaskReport {
            name: "build".to_string(),
            status: TaskStatus::Success,
            duration_ms: 1234,
            exit_code: Some(0),
            inputs_matched: vec!["src/main.rs".to_string()],
            cache_key: Some("abc123".to_string()),
            outputs: vec!["target/release/app".to_string()],
        };
        let json = serde_json::to_string(&report).unwrap();
        let parsed: TaskReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "build");
        assert_eq!(parsed.status, TaskStatus::Success);
        assert_eq!(parsed.duration_ms, 1234);
        assert_eq!(parsed.exit_code, Some(0));
    }

    #[test]
    fn test_task_report_with_no_exit_code() {
        let report = TaskReport {
            name: "test".to_string(),
            status: TaskStatus::Skipped,
            duration_ms: 0,
            exit_code: None,
            inputs_matched: vec![],
            cache_key: None,
            outputs: vec![],
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains(r#""exit_code":null"#));
    }

    #[test]
    fn test_context_report_serde() {
        let context = ContextReport {
            provider: "github".to_string(),
            event: "push".to_string(),
            ref_name: "refs/heads/main".to_string(),
            base_ref: Some("refs/heads/develop".to_string()),
            sha: "abc123def456".to_string(),
            changed_files: vec!["src/lib.rs".to_string(), "Cargo.toml".to_string()],
        };
        let json = serde_json::to_string(&context).unwrap();
        let parsed: ContextReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.provider, "github");
        assert_eq!(parsed.changed_files.len(), 2);
    }

    #[test]
    fn test_context_report_no_base_ref() {
        let context = ContextReport {
            provider: "buildkite".to_string(),
            event: "manual".to_string(),
            ref_name: "main".to_string(),
            base_ref: None,
            sha: "deadbeef".to_string(),
            changed_files: vec![],
        };
        let json = serde_json::to_string(&context).unwrap();
        assert!(json.contains(r#""base_ref":null"#));
    }

    #[test]
    fn test_pipeline_report_cache_hits() {
        let report = PipelineReport {
            version: "1.0.0".to_string(),
            project: "test-project".to_string(),
            pipeline: "default".to_string(),
            context: ContextReport {
                provider: "local".to_string(),
                event: "push".to_string(),
                ref_name: "main".to_string(),
                base_ref: None,
                sha: "abc123".to_string(),
                changed_files: vec![],
            },
            started_at: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
            completed_at: Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 1, 0).unwrap()),
            duration_ms: Some(60000),
            status: PipelineStatus::Success,
            tasks: vec![
                TaskReport {
                    name: "build".to_string(),
                    status: TaskStatus::Cached,
                    duration_ms: 100,
                    exit_code: Some(0),
                    inputs_matched: vec![],
                    cache_key: Some("key1".to_string()),
                    outputs: vec![],
                },
                TaskReport {
                    name: "test".to_string(),
                    status: TaskStatus::Success,
                    duration_ms: 5000,
                    exit_code: Some(0),
                    inputs_matched: vec![],
                    cache_key: Some("key2".to_string()),
                    outputs: vec![],
                },
                TaskReport {
                    name: "lint".to_string(),
                    status: TaskStatus::Cached,
                    duration_ms: 50,
                    exit_code: Some(0),
                    inputs_matched: vec![],
                    cache_key: Some("key3".to_string()),
                    outputs: vec![],
                },
            ],
        };
        assert_eq!(report.cache_hits(), 2);
    }

    #[test]
    fn test_pipeline_report_no_cache_hits() {
        let report = PipelineReport {
            version: "1.0.0".to_string(),
            project: "test".to_string(),
            pipeline: "ci".to_string(),
            context: ContextReport {
                provider: "local".to_string(),
                event: "push".to_string(),
                ref_name: "main".to_string(),
                base_ref: None,
                sha: "abc".to_string(),
                changed_files: vec![],
            },
            started_at: Utc::now(),
            completed_at: None,
            duration_ms: None,
            status: PipelineStatus::Pending,
            tasks: vec![TaskReport {
                name: "build".to_string(),
                status: TaskStatus::Success,
                duration_ms: 1000,
                exit_code: Some(0),
                inputs_matched: vec![],
                cache_key: None,
                outputs: vec![],
            }],
        };
        assert_eq!(report.cache_hits(), 0);
    }

    #[test]
    fn test_pipeline_report_serde_roundtrip() {
        let report = PipelineReport {
            version: "0.21.0".to_string(),
            project: "/path/to/project".to_string(),
            pipeline: "ci".to_string(),
            context: ContextReport {
                provider: "github".to_string(),
                event: "pull_request".to_string(),
                ref_name: "refs/pull/123/merge".to_string(),
                base_ref: Some("main".to_string()),
                sha: "abc123".to_string(),
                changed_files: vec!["file.rs".to_string()],
            },
            started_at: Utc.with_ymd_and_hms(2024, 6, 15, 12, 0, 0).unwrap(),
            completed_at: Some(Utc.with_ymd_and_hms(2024, 6, 15, 12, 5, 0).unwrap()),
            duration_ms: Some(300_000),
            status: PipelineStatus::Success,
            tasks: vec![],
        };
        let json = serde_json::to_string_pretty(&report).unwrap();
        let parsed: PipelineReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.version, report.version);
        assert_eq!(parsed.project, report.project);
        assert_eq!(parsed.status, PipelineStatus::Success);
    }

    #[test]
    fn test_check_handle_debug() {
        let handle = CheckHandle {
            id: "12345".to_string(),
        };
        let debug = format!("{:?}", handle);
        assert!(debug.contains("12345"));
    }

    #[test]
    fn test_check_handle_clone() {
        let handle = CheckHandle {
            id: "abc".to_string(),
        };
        let cloned = handle.clone();
        assert_eq!(cloned.id, "abc");
    }

    #[test]
    fn test_task_status_equality() {
        assert_eq!(TaskStatus::Success, TaskStatus::Success);
        assert_ne!(TaskStatus::Success, TaskStatus::Failed);
        assert_eq!(TaskStatus::Cached, TaskStatus::Cached);
    }

    #[test]
    fn test_pipeline_status_equality() {
        assert_eq!(PipelineStatus::Success, PipelineStatus::Success);
        assert_ne!(PipelineStatus::Success, PipelineStatus::Failed);
        assert_ne!(PipelineStatus::Pending, PipelineStatus::Partial);
    }

    #[test]
    fn test_task_report_clone() {
        let report = TaskReport {
            name: "test".to_string(),
            status: TaskStatus::Success,
            duration_ms: 100,
            exit_code: Some(0),
            inputs_matched: vec!["a.rs".to_string()],
            cache_key: Some("key".to_string()),
            outputs: vec!["out".to_string()],
        };
        let cloned = report.clone();
        assert_eq!(cloned.name, "test");
        assert_eq!(cloned.inputs_matched.len(), 1);
    }

    #[test]
    fn test_context_report_clone() {
        let context = ContextReport {
            provider: "github".to_string(),
            event: "push".to_string(),
            ref_name: "main".to_string(),
            base_ref: None,
            sha: "abc".to_string(),
            changed_files: vec!["file.rs".to_string()],
        };
        let cloned = context.clone();
        assert_eq!(cloned.provider, "github");
        assert_eq!(cloned.changed_files.len(), 1);
    }
}
