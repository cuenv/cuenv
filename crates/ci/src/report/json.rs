use super::PipelineReport;
use cuenv_core::Result;

/// Writes the pipeline report to a JSON file
///
/// # Errors
/// Returns error if file creation or JSON serialization fails
pub fn write_report(report: &PipelineReport, path: &std::path::Path) -> Result<()> {
    let file = std::fs::File::create(path)?;
    serde_json::to_writer_pretty(file, report).map_err(|e| cuenv_core::Error::Io {
        source: e.into(),
        path: Some(path.into()),
        operation: "write_report".to_string(),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{ContextReport, PipelineStatus, TaskReport, TaskStatus};
    use chrono::Utc;
    use tempfile::TempDir;

    fn create_test_report() -> PipelineReport {
        PipelineReport {
            version: "0.21.4".to_string(),
            project: "test-project".to_string(),
            pipeline: "default".to_string(),
            context: ContextReport {
                provider: "github".to_string(),
                event: "push".to_string(),
                ref_name: "refs/heads/main".to_string(),
                base_ref: None,
                sha: "abc123".to_string(),
                changed_files: vec!["src/main.rs".to_string()],
            },
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            duration_ms: Some(1234),
            status: PipelineStatus::Success,
            tasks: vec![TaskReport {
                name: "build".to_string(),
                status: TaskStatus::Success,
                duration_ms: 500,
                exit_code: Some(0),
                inputs_matched: vec!["src/**/*.rs".to_string()],
                cache_key: Some("abc123".to_string()),
                outputs: vec!["target/release/binary".to_string()],
            }],
        }
    }

    #[test]
    fn test_write_report_creates_valid_json() {
        let temp_dir = TempDir::new().unwrap();
        let report_path = temp_dir.path().join("report.json");
        let report = create_test_report();

        let result = write_report(&report, &report_path);
        assert!(result.is_ok());

        // Verify file was created
        assert!(report_path.exists());

        // Read and parse the file
        let content = std::fs::read_to_string(&report_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        // Verify expected fields
        assert_eq!(parsed["version"], "0.21.4");
        assert_eq!(parsed["project"], "test-project");
        assert_eq!(parsed["pipeline"], "default");
        assert_eq!(parsed["status"], "success");
    }

    #[test]
    fn test_write_report_pretty_prints() {
        let temp_dir = TempDir::new().unwrap();
        let report_path = temp_dir.path().join("report.json");
        let report = create_test_report();

        write_report(&report, &report_path).unwrap();

        // Pretty-printed JSON should contain indentation
        let content = std::fs::read_to_string(&report_path).unwrap();
        assert!(content.contains('\n'));
        assert!(content.contains("  "));
    }

    #[test]
    fn test_write_report_includes_context() {
        let temp_dir = TempDir::new().unwrap();
        let report_path = temp_dir.path().join("report.json");
        let report = create_test_report();

        write_report(&report, &report_path).unwrap();

        let file_content = std::fs::read_to_string(&report_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&file_content).unwrap();

        let ctx = &parsed["context"];
        assert_eq!(ctx["provider"], "github");
        assert_eq!(ctx["event"], "push");
        assert_eq!(ctx["ref_name"], "refs/heads/main");
        assert_eq!(ctx["sha"], "abc123");
    }

    #[test]
    fn test_write_report_includes_tasks() {
        let temp_dir = TempDir::new().unwrap();
        let report_path = temp_dir.path().join("report.json");
        let report = create_test_report();

        write_report(&report, &report_path).unwrap();

        let content = std::fs::read_to_string(&report_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        let tasks = parsed["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["name"], "build");
        assert_eq!(tasks[0]["status"], "success");
        assert_eq!(tasks[0]["duration_ms"], 500);
    }

    #[test]
    fn test_write_report_failed_pipeline() {
        let temp_dir = TempDir::new().unwrap();
        let report_path = temp_dir.path().join("report.json");
        let mut report = create_test_report();
        report.status = PipelineStatus::Failed;
        report.tasks[0].status = TaskStatus::Failed;
        report.tasks[0].exit_code = Some(1);

        write_report(&report, &report_path).unwrap();

        let content = std::fs::read_to_string(&report_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert_eq!(parsed["status"], "failed");
        assert_eq!(parsed["tasks"][0]["status"], "failed");
        assert_eq!(parsed["tasks"][0]["exit_code"], 1);
    }

    #[test]
    fn test_write_report_cached_task() {
        let temp_dir = TempDir::new().unwrap();
        let report_path = temp_dir.path().join("report.json");
        let mut report = create_test_report();
        report.tasks[0].status = TaskStatus::Cached;

        write_report(&report, &report_path).unwrap();

        let content = std::fs::read_to_string(&report_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert_eq!(parsed["tasks"][0]["status"], "cached");
    }

    #[test]
    fn test_write_report_to_nested_directory() {
        let temp_dir = TempDir::new().unwrap();
        let nested_path = temp_dir.path().join("nested").join("dir");
        std::fs::create_dir_all(&nested_path).unwrap();
        let report_path = nested_path.join("report.json");
        let report = create_test_report();

        let result = write_report(&report, &report_path);
        assert!(result.is_ok());
        assert!(report_path.exists());
    }

    #[test]
    fn test_write_report_invalid_path_fails() {
        let report = create_test_report();
        // Path to a non-existent directory
        let invalid_path = std::path::Path::new("/nonexistent/dir/report.json");

        let result = write_report(&report, invalid_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_write_report_multiple_tasks() {
        let temp_dir = TempDir::new().unwrap();
        let report_path = temp_dir.path().join("report.json");
        let mut report = create_test_report();
        report.tasks.push(TaskReport {
            name: "test".to_string(),
            status: TaskStatus::Success,
            duration_ms: 300,
            exit_code: Some(0),
            inputs_matched: vec!["tests/**/*.rs".to_string()],
            cache_key: None,
            outputs: vec![],
        });
        report.tasks.push(TaskReport {
            name: "lint".to_string(),
            status: TaskStatus::Skipped,
            duration_ms: 0,
            exit_code: None,
            inputs_matched: vec![],
            cache_key: None,
            outputs: vec![],
        });

        write_report(&report, &report_path).unwrap();

        let content = std::fs::read_to_string(&report_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        let tasks = parsed["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[1]["name"], "test");
        assert_eq!(tasks[2]["name"], "lint");
        assert_eq!(tasks[2]["status"], "skipped");
    }
}
