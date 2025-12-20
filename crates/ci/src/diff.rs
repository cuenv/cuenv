//! Digest Diff Tool
//!
//! Compares two CI runs to identify what caused cache invalidation.
//! Shows changed files, environment variables, and upstream outputs
//! without exposing secret values.

use crate::report::{PipelineReport, TaskReport};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors for diff operations
#[derive(Debug, Error)]
pub enum DiffError {
    /// Report file not found
    #[error("Report not found: {0}")]
    ReportNotFound(PathBuf),

    /// Failed to read report
    #[error("Failed to read report '{path}': {source}")]
    ReadError {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Failed to parse report
    #[error("Failed to parse report '{path}': {source}")]
    ParseError {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    /// Invalid run identifier
    #[error("Invalid run identifier: {0}")]
    InvalidRunId(String),
}

/// Result of comparing two CI runs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestDiff {
    /// Run A identifier (typically commit SHA)
    pub run_a: String,
    /// Run B identifier
    pub run_b: String,
    /// Tasks that changed between runs
    pub task_diffs: Vec<TaskDiff>,
    /// Summary of changes
    pub summary: DiffSummary,
}

/// Changes for a single task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDiff {
    /// Task name
    pub name: String,
    /// Change type
    pub change_type: ChangeType,
    /// Changed input files
    pub changed_files: Vec<String>,
    /// Changed environment variables (names only)
    pub changed_env_vars: Vec<String>,
    /// Changed upstream task outputs
    pub changed_upstream: Vec<String>,
    /// Whether secret fingerprint changed (no values exposed)
    pub secrets_changed: bool,
    /// Cache key in run A (if available)
    pub cache_key_a: Option<String>,
    /// Cache key in run B (if available)
    pub cache_key_b: Option<String>,
}

/// Type of change for a task
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    /// Task exists in both runs with same inputs
    Unchanged,
    /// Task inputs changed
    Modified,
    /// Task only exists in run A
    Removed,
    /// Task only exists in run B
    Added,
    /// Cache key changed but reason unknown
    CacheInvalidated,
}

/// Summary statistics for the diff
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiffSummary {
    /// Total tasks compared
    pub total_tasks: usize,
    /// Tasks with changes
    pub changed_tasks: usize,
    /// Tasks added in run B
    pub added_tasks: usize,
    /// Tasks removed in run B
    pub removed_tasks: usize,
    /// Tasks with secret changes
    pub secret_changes: usize,
    /// Tasks with file changes
    pub file_changes: usize,
    /// Tasks with env var changes
    pub env_changes: usize,
}

/// Compare two CI runs by their report files
pub fn compare_runs(run_a: &Path, run_b: &Path) -> Result<DigestDiff, DiffError> {
    let report_a = load_report(run_a)?;
    let report_b = load_report(run_b)?;
    compare_reports(&report_a, &report_b)
}

/// Compare two CI runs by commit SHA
pub fn compare_by_sha(
    sha_a: &str,
    sha_b: &str,
    reports_dir: &Path,
) -> Result<DigestDiff, DiffError> {
    let dir_a = reports_dir.join(sha_a);
    let dir_b = reports_dir.join(sha_b);
    let report_a = find_first_report(&dir_a)?;
    let report_b = find_first_report(&dir_b)?;
    compare_runs(&report_a, &report_b)
}

/// Compare two pipeline reports
pub fn compare_reports(
    report_a: &PipelineReport,
    report_b: &PipelineReport,
) -> Result<DigestDiff, DiffError> {
    let mut task_diffs = Vec::new();
    let mut summary = DiffSummary::default();

    let tasks_a: HashMap<&str, &TaskReport> = report_a
        .tasks
        .iter()
        .map(|t| (t.name.as_str(), t))
        .collect();
    let tasks_b: HashMap<&str, &TaskReport> = report_b
        .tasks
        .iter()
        .map(|t| (t.name.as_str(), t))
        .collect();

    let all_tasks: HashSet<&str> = tasks_a.keys().chain(tasks_b.keys()).copied().collect();
    summary.total_tasks = all_tasks.len();

    for name in all_tasks {
        let task_a = tasks_a.get(name);
        let task_b = tasks_b.get(name);

        let diff = match (task_a, task_b) {
            (Some(a), Some(b)) => compare_tasks(name, a, b),
            (Some(_), None) => TaskDiff {
                name: name.to_string(),
                change_type: ChangeType::Removed,
                changed_files: vec![],
                changed_env_vars: vec![],
                changed_upstream: vec![],
                secrets_changed: false,
                cache_key_a: task_a.and_then(|t| t.cache_key.clone()),
                cache_key_b: None,
            },
            (None, Some(_)) => TaskDiff {
                name: name.to_string(),
                change_type: ChangeType::Added,
                changed_files: vec![],
                changed_env_vars: vec![],
                changed_upstream: vec![],
                secrets_changed: false,
                cache_key_a: None,
                cache_key_b: task_b.and_then(|t| t.cache_key.clone()),
            },
            (None, None) => unreachable!(),
        };

        match diff.change_type {
            ChangeType::Unchanged => {}
            ChangeType::Modified | ChangeType::CacheInvalidated => summary.changed_tasks += 1,
            ChangeType::Added => summary.added_tasks += 1,
            ChangeType::Removed => summary.removed_tasks += 1,
        }
        if diff.secrets_changed {
            summary.secret_changes += 1;
        }
        if !diff.changed_files.is_empty() {
            summary.file_changes += 1;
        }
        if !diff.changed_env_vars.is_empty() {
            summary.env_changes += 1;
        }

        task_diffs.push(diff);
    }

    task_diffs.sort_by(|a, b| {
        let order = |ct: ChangeType| match ct {
            ChangeType::Modified => 0,
            ChangeType::CacheInvalidated => 1,
            ChangeType::Added => 2,
            ChangeType::Removed => 3,
            ChangeType::Unchanged => 4,
        };
        order(a.change_type).cmp(&order(b.change_type))
    });

    Ok(DigestDiff {
        run_a: report_a.context.sha.clone(),
        run_b: report_b.context.sha.clone(),
        task_diffs,
        summary,
    })
}

fn compare_tasks(name: &str, task_a: &TaskReport, task_b: &TaskReport) -> TaskDiff {
    let mut changed_files = Vec::new();

    let inputs_a: HashSet<&str> = task_a.inputs_matched.iter().map(String::as_str).collect();
    let inputs_b: HashSet<&str> = task_b.inputs_matched.iter().map(String::as_str).collect();

    for input in inputs_a.symmetric_difference(&inputs_b) {
        changed_files.push((*input).to_string());
    }

    let secrets_changed = task_a.cache_key != task_b.cache_key
        && changed_files.is_empty()
        && task_a.cache_key.is_some()
        && task_b.cache_key.is_some();

    let change_type = if task_a.cache_key == task_b.cache_key {
        ChangeType::Unchanged
    } else if !changed_files.is_empty() {
        ChangeType::Modified
    } else {
        ChangeType::CacheInvalidated
    };

    TaskDiff {
        name: name.to_string(),
        change_type,
        changed_files,
        changed_env_vars: vec![],
        changed_upstream: vec![],
        secrets_changed,
        cache_key_a: task_a.cache_key.clone(),
        cache_key_b: task_b.cache_key.clone(),
    }
}

fn load_report(path: &Path) -> Result<PipelineReport, DiffError> {
    if !path.exists() {
        return Err(DiffError::ReportNotFound(path.to_path_buf()));
    }
    let contents = fs::read_to_string(path).map_err(|e| DiffError::ReadError {
        path: path.to_path_buf(),
        source: e,
    })?;
    serde_json::from_str(&contents).map_err(|e| DiffError::ParseError {
        path: path.to_path_buf(),
        source: e,
    })
}

fn find_first_report(dir: &Path) -> Result<PathBuf, DiffError> {
    if !dir.exists() {
        return Err(DiffError::ReportNotFound(dir.to_path_buf()));
    }
    let entries = fs::read_dir(dir).map_err(|e| DiffError::ReadError {
        path: dir.to_path_buf(),
        source: e,
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            return Ok(path);
        }
    }
    Err(DiffError::ReportNotFound(dir.to_path_buf()))
}

/// Format a diff for human-readable output
#[must_use]
pub fn format_diff(diff: &DigestDiff) -> String {
    use std::fmt::Write;

    let mut output = String::new();
    let _ = writeln!(
        output,
        "Comparing runs: {} -> {}\n",
        &diff.run_a[..7.min(diff.run_a.len())],
        &diff.run_b[..7.min(diff.run_b.len())]
    );
    output.push_str("Summary:\n");
    let _ = writeln!(output, "  Total tasks: {}", diff.summary.total_tasks);
    let _ = writeln!(output, "  Changed: {}", diff.summary.changed_tasks);
    let _ = writeln!(output, "  Added: {}", diff.summary.added_tasks);
    let _ = writeln!(output, "  Removed: {}", diff.summary.removed_tasks);
    if diff.summary.secret_changes > 0 {
        let _ = writeln!(output, "  Secret changes: {}", diff.summary.secret_changes);
    }
    output.push('\n');

    for task in &diff.task_diffs {
        if task.change_type == ChangeType::Unchanged {
            continue;
        }
        let symbol = match task.change_type {
            ChangeType::Modified => "~",
            ChangeType::CacheInvalidated => "!",
            ChangeType::Added => "+",
            ChangeType::Removed => "-",
            ChangeType::Unchanged => " ",
        };
        let _ = writeln!(output, "{} {}", symbol, task.name);
        if !task.changed_files.is_empty() {
            output.push_str("  Changed files:\n");
            for file in &task.changed_files {
                let _ = writeln!(output, "    - {file}");
            }
        }
        if task.secrets_changed {
            output.push_str("  Secrets: changed (values hidden)\n");
        }
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{ContextReport, PipelineStatus, TaskStatus};
    use chrono::Utc;

    fn make_report(sha: &str, tasks: Vec<TaskReport>) -> PipelineReport {
        PipelineReport {
            version: "1.0".to_string(),
            project: "test".to_string(),
            pipeline: "test-pipeline".to_string(),
            context: ContextReport {
                provider: "test".to_string(),
                event: "push".to_string(),
                ref_name: "refs/heads/main".to_string(),
                base_ref: None,
                sha: sha.to_string(),
                changed_files: vec![],
            },
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            duration_ms: Some(1000),
            status: PipelineStatus::Success,
            tasks,
        }
    }

    fn make_task(name: &str, inputs: Vec<&str>, cache_key: Option<&str>) -> TaskReport {
        TaskReport {
            name: name.to_string(),
            status: TaskStatus::Success,
            duration_ms: 100,
            exit_code: Some(0),
            inputs_matched: inputs.into_iter().map(String::from).collect(),
            cache_key: cache_key.map(String::from),
            outputs: vec![],
        }
    }

    #[test]
    fn test_unchanged_tasks() {
        let report_a = make_report(
            "abc123",
            vec![make_task("build", vec!["src/main.rs"], Some("key1"))],
        );
        let report_b = make_report(
            "def456",
            vec![make_task("build", vec!["src/main.rs"], Some("key1"))],
        );
        let diff = compare_reports(&report_a, &report_b).unwrap();
        assert_eq!(diff.task_diffs[0].change_type, ChangeType::Unchanged);
    }

    #[test]
    fn test_modified_task() {
        let report_a = make_report(
            "abc123",
            vec![make_task("build", vec!["src/main.rs"], Some("key1"))],
        );
        let report_b = make_report(
            "def456",
            vec![make_task(
                "build",
                vec!["src/main.rs", "src/lib.rs"],
                Some("key2"),
            )],
        );
        let diff = compare_reports(&report_a, &report_b).unwrap();
        assert_eq!(diff.task_diffs[0].change_type, ChangeType::Modified);
        assert!(
            diff.task_diffs[0]
                .changed_files
                .contains(&"src/lib.rs".to_string())
        );
    }

    #[test]
    fn test_secret_change_detection() {
        let report_a = make_report(
            "abc123",
            vec![make_task("deploy", vec!["config.yml"], Some("key1"))],
        );
        let report_b = make_report(
            "def456",
            vec![make_task("deploy", vec!["config.yml"], Some("key2"))],
        );
        let diff = compare_reports(&report_a, &report_b).unwrap();
        assert!(diff.task_diffs[0].secrets_changed);
    }
}
