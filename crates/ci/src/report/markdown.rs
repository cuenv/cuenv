use super::{PipelineReport, PipelineStatus, TaskStatus};
use std::fmt::Write;

/// Generate a markdown summary of the pipeline report.
///
/// This is used for PR comments, GitHub Check Run summaries, and Job Summaries.
#[must_use]
pub fn generate_summary(report: &PipelineReport) -> String {
    let mut md = String::new();

    // Header with status emoji
    let (status_emoji, status_text) = match report.status {
        PipelineStatus::Success => ("\u{2705}", "Success"), // ✅
        PipelineStatus::Failed => ("\u{274C}", "Failed"),   // ❌
        PipelineStatus::Partial => ("\u{26A0}\u{FE0F}", "Partial"), // ⚠️
        PipelineStatus::Pending => ("\u{23F3}", "Pending"), // ⏳
    };

    let _ = writeln!(md, "## {status_emoji} cuenv CI Report - {status_text}\n");

    // Summary table
    let duration = report
        .duration_ms
        .map_or_else(|| "—".to_string(), format_duration);

    md.push_str("| Project | Pipeline | Status | Duration |\n");
    md.push_str("|:--------|:---------|:------:|:--------:|\n");
    let _ = writeln!(
        md,
        "| `{}` | `{}` | {status_emoji} {status_text} | {duration} |\n",
        report.project, report.pipeline
    );

    // Tasks table (if any)
    if !report.tasks.is_empty() {
        md.push_str("### Tasks\n\n");
        md.push_str("| Task | Status | Duration |\n");
        md.push_str("|:-----|:------:|:--------:|\n");

        for task in &report.tasks {
            let (task_emoji, task_status) = match task.status {
                TaskStatus::Success => ("\u{2705}", "Passed"), // ✅
                TaskStatus::Failed => ("\u{274C}", "Failed"),  // ❌
                TaskStatus::Cached => ("\u{1F4BE}", "Cached"), // 💾
                TaskStatus::Skipped => ("\u{23ED}\u{FE0F}", "Skipped"), // ⏭️
            };
            let task_duration = format_duration(task.duration_ms);
            let _ = writeln!(
                md,
                "| `{}` | {task_emoji} {task_status} | {task_duration} |",
                task.name
            );
        }
        md.push('\n');
    }

    // Annotations (if any) - sorted by label for deterministic output
    if !report.annotations.is_empty() {
        let mut sorted: Vec<_> = report.annotations.iter().collect();
        sorted.sort_by_key(|(label, _)| *label);

        md.push_str("### Annotations\n\n");
        md.push_str("| Label | Value |\n|:------|:------|\n");
        for (label, value) in &sorted {
            let escaped_label = escape_table_cell(label);
            let escaped_value = escape_table_cell(value);
            let display =
                if escaped_value.starts_with("https://") || escaped_value.starts_with("http://") {
                    format!("[{escaped_value}]({escaped_value})")
                } else {
                    escaped_value
                };
            let _ = writeln!(md, "| {escaped_label} | {display} |");
        }
        md.push('\n');
    }

    // Context details
    md.push_str("### Details\n\n");
    md.push_str("| Property | Value |\n|:---------|:------|\n");
    let _ = writeln!(
        md,
        "| Commit | `{}` |",
        &report.context.sha[..8.min(report.context.sha.len())]
    );
    let _ = writeln!(md, "| Ref | `{}` |", report.context.ref_name);
    if let Some(base_ref) = &report.context.base_ref {
        let _ = writeln!(md, "| Base | `{base_ref}` |");
    }
    let _ = writeln!(
        md,
        "| Changed files | {} |",
        report.context.changed_files.len()
    );
    let _ = writeln!(md, "| Provider | {} |", report.context.provider);

    // Footer
    let _ = write!(md, "\n---\n*cuenv v{}*\n", report.version);

    md
}

/// Known CI system environment variables for job summary output.
///
/// Each CI system has its own mechanism for displaying job summaries:
/// - GitHub Actions: `GITHUB_STEP_SUMMARY` - append markdown to this file
/// - GitLab CI: `CI_JOB_URL` - no native summary, but we could post to MR (future)
/// - Buildkite: `BUILDKITE_ANNOTATION_CONTEXT` - use buildkite-agent annotate (future)
const JOB_SUMMARY_ENV_VARS: &[&str] = &[
    "GITHUB_STEP_SUMMARY", // GitHub Actions
                           // Future: Add other CI systems as they're implemented
];

/// Write the summary to the CI system's job summary mechanism.
///
/// Uses runtime detection to find the appropriate summary file/mechanism.
/// Currently supports:
/// - GitHub Actions: writes to `$GITHUB_STEP_SUMMARY`
///
/// Appends to the file to support multiple projects in a single run.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or written to.
pub fn write_job_summary(report: &PipelineReport) -> std::io::Result<()> {
    use std::io::Write as IoWrite;

    // Try each known CI system's summary mechanism
    for env_var in JOB_SUMMARY_ENV_VARS {
        if let Ok(path) = std::env::var(env_var) {
            let summary = generate_summary(report);
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            writeln!(file, "{summary}")?;
            tracing::info!("Wrote job summary to {path} (via {env_var})");
            return Ok(());
        }
    }

    // No summary mechanism available - this is not an error
    tracing::debug!(
        "No job summary mechanism available (checked: {:?})",
        JOB_SUMMARY_ENV_VARS
    );
    Ok(())
}

/// Escape characters that would break markdown table rendering.
fn escape_table_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

/// Format duration in milliseconds to a human-readable string.
#[allow(clippy::cast_precision_loss)]
fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let minutes = ms / 60_000;
        let seconds = (ms % 60_000) / 1000;
        format!("{minutes}m {seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{ContextReport, TaskReport};
    use chrono::Utc;
    use std::collections::HashMap;

    #[test]
    fn test_generate_summary_success() {
        let report = PipelineReport {
            version: "0.11.8".to_string(),
            project: "my-project".to_string(),
            pipeline: "ci".to_string(),
            context: ContextReport {
                provider: "github".to_string(),
                event: "pull_request".to_string(),
                ref_name: "refs/pull/123/merge".to_string(),
                base_ref: Some("main".to_string()),
                sha: "abc123def456".to_string(),
                changed_files: vec!["src/lib.rs".to_string()],
            },
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            duration_ms: Some(5432),
            status: PipelineStatus::Success,
            tasks: vec![TaskReport {
                name: "check".to_string(),
                status: TaskStatus::Success,
                duration_ms: 5000,
                exit_code: Some(0),
                inputs_matched: vec![],
                cache_key: None,
                outputs: vec![],
                captures: HashMap::new(),
            }],
            annotations: HashMap::new(),
        };

        let md = generate_summary(&report);
        assert!(md.contains("\u{2705}")); // ✅
        assert!(md.contains("my-project"));
        assert!(md.contains("check"));
        assert!(md.contains("abc123de"));
        assert!(md.contains("Success"));
    }

    #[test]
    fn test_generate_summary_failed() {
        let report = PipelineReport {
            version: "0.11.8".to_string(),
            project: "my-project".to_string(),
            pipeline: "ci".to_string(),
            context: ContextReport {
                provider: "github".to_string(),
                event: "pull_request".to_string(),
                ref_name: "refs/pull/123/merge".to_string(),
                base_ref: Some("main".to_string()),
                sha: "abc123def456".to_string(),
                changed_files: vec!["src/lib.rs".to_string()],
            },
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            duration_ms: Some(5432),
            status: PipelineStatus::Failed,
            tasks: vec![TaskReport {
                name: "check".to_string(),
                status: TaskStatus::Failed,
                duration_ms: 5000,
                exit_code: Some(1),
                inputs_matched: vec![],
                cache_key: None,
                outputs: vec![],
                captures: HashMap::new(),
            }],
            annotations: HashMap::new(),
        };

        let md = generate_summary(&report);
        assert!(md.contains("\u{274C}")); // ❌
        assert!(md.contains("Failed"));
    }

    #[test]
    fn test_generate_summary_with_annotations() {
        let mut annotations = HashMap::new();
        annotations.insert(
            "Preview URL".to_string(),
            "https://preview.example.workers.dev".to_string(),
        );
        annotations.insert("Version".to_string(), "1.2.3".to_string());

        let report = PipelineReport {
            version: "0.11.8".to_string(),
            project: "my-project".to_string(),
            pipeline: "pullRequest".to_string(),
            context: ContextReport {
                provider: "github".to_string(),
                event: "pull_request".to_string(),
                ref_name: "refs/pull/42/merge".to_string(),
                base_ref: Some("main".to_string()),
                sha: "abc123def456".to_string(),
                changed_files: vec![],
            },
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            duration_ms: Some(1000),
            status: PipelineStatus::Success,
            tasks: vec![],
            annotations,
        };

        let md = generate_summary(&report);
        assert!(md.contains("### Annotations"));
        assert!(md.contains("Preview URL"));
        // URLs should be auto-linked
        assert!(md.contains(
            "[https://preview.example.workers.dev](https://preview.example.workers.dev)"
        ));
        // Non-URL values should be plain
        assert!(md.contains("| Version | 1.2.3 |"));
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(500), "500ms");
        assert_eq!(format_duration(1500), "1.5s");
        assert_eq!(format_duration(65000), "1m 5s");
    }
}
