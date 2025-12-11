use super::{PipelineReport, PipelineStatus, TaskStatus};

/// Generate a markdown summary of the pipeline report.
///
/// This is used for both PR comments and GitHub Check Run summaries.
#[must_use]
pub fn generate_summary(report: &PipelineReport) -> String {
    let mut md = String::new();

    // Header with status emoji
    let status_emoji = match report.status {
        PipelineStatus::Success => "",
        PipelineStatus::Failed => "",
        PipelineStatus::Partial => "",
        PipelineStatus::Pending => "",
    };

    md.push_str(&format!("## {} cuenv CI Report\n\n", status_emoji));

    // Summary table
    let duration = report
        .duration_ms
        .map_or_else(|| "—".to_string(), format_duration);

    md.push_str("| Project | Pipeline | Status | Duration |\n");
    md.push_str("|---------|----------|--------|----------|\n");
    md.push_str(&format!(
        "| `{}` | `{}` | {} | {} |\n\n",
        report.project, report.pipeline, status_emoji, duration
    ));

    // Tasks table (if any)
    if !report.tasks.is_empty() {
        md.push_str("### Tasks\n\n");
        md.push_str("| Task | Status | Duration |\n");
        md.push_str("|------|--------|----------|\n");

        for task in &report.tasks {
            let task_emoji = match task.status {
                TaskStatus::Success => "",
                TaskStatus::Failed => "",
                TaskStatus::Cached => "",
                TaskStatus::Skipped => "",
            };
            let task_duration = format_duration(task.duration_ms);
            md.push_str(&format!(
                "| `{}` | {} | {} |\n",
                task.name, task_emoji, task_duration
            ));
        }
        md.push('\n');
    }

    // Context details
    md.push_str("### Details\n\n");
    md.push_str(&format!("- **Commit:** `{}`\n", &report.context.sha[..8.min(report.context.sha.len())]));
    md.push_str(&format!("- **Ref:** `{}`\n", report.context.ref_name));
    if let Some(base_ref) = &report.context.base_ref {
        md.push_str(&format!("- **Base:** `{}`\n", base_ref));
    }
    md.push_str(&format!(
        "- **Changed files:** {}\n",
        report.context.changed_files.len()
    ));

    // Footer
    md.push_str(&format!("\n---\n*cuenv v{}*\n", report.version));

    md
}

/// Format duration in milliseconds to a human-readable string.
fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let minutes = ms / 60_000;
        let seconds = (ms % 60_000) / 1000;
        format!("{}m {}s", minutes, seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{ContextReport, TaskReport};
    use chrono::Utc;

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
            }],
        };

        let md = generate_summary(&report);
        assert!(md.contains(""));
        assert!(md.contains("my-project"));
        assert!(md.contains("check"));
        assert!(md.contains("abc123de"));
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(500), "500ms");
        assert_eq!(format_duration(1500), "1.5s");
        assert_eq!(format_duration(65000), "1m 5s");
    }
}
