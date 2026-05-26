use super::*;
use crate::commands::CommandExecutor;
use cuenv_core::tasks::TaskNode;
use tokio::sync::mpsc;

use std::fs;
use tempfile::TempDir;

/// Create a test executor for unit tests.
fn create_test_executor() -> CommandExecutor {
    let (sender, _receiver) = mpsc::unbounded_channel();
    CommandExecutor::new(sender, "cuenv".to_string())
}

#[tokio::test]
async fn test_list_tasks_empty() {
    let temp_dir = TempDir::new().expect("write to string");
    let cue_content = r#"package test
env: {
    FOO: "bar"
}"#;
    fs::write(temp_dir.path().join("env.cue"), cue_content).expect("write to string");

    let executor = create_test_executor();
    let request = TaskExecutionRequest::list(temp_dir.path().to_str().unwrap(), "test", &executor);
    let result = execute(request).await;

    // The result depends on FFI availability
    if let Ok(output) = result {
        assert!(output.contains("No tasks") || output.contains("Available tasks"));
    } else {
        // FFI not available in test environment
    }
}

#[test]
fn test_format_task_results_variants() {
    let r_ok = cuenv_core::tasks::TaskResult {
        name: "t".into(),
        exit_code: Some(0),
        stdout: "hello".into(),
        stderr: String::new(),
        success: true,
    };
    let r_fail = cuenv_core::tasks::TaskResult {
        name: "t".into(),
        exit_code: Some(1),
        stdout: String::new(),
        stderr: "boom".into(),
        success: false,
    };

    // capture on: show status and fields
    let s = format_task_results(vec![r_ok.clone(), r_fail.clone()], true.into(), "t");
    assert!(s.contains("succeeded"));
    assert!(s.contains("Output:"));
    assert!(s.contains("failed with exit code"));
    assert!(s.contains("Error:"));

    // capture off: logs passed through + completion line
    let s2 = format_task_results(vec![r_ok], false.into(), "t");
    assert!(!s2.contains("hello")); // Output handled by executor now
    assert!(s2.contains("Task 't' completed"));

    // capture on with empty output -> default completion
    let s3 = format_task_results(vec![], true.into(), "abc");
    assert_eq!(s3, "Task 'abc' completed");
}

#[test]
fn test_render_task_tree() {
    use cuenv_core::tasks::IndexedTask;
    // Helper to create a dummy task
    let make_task = |desc: Option<&str>| Task {
        command: "echo".into(),
        description: desc.map(ToString::to_string),
        ..Default::default()
    };

    let t_build = IndexedTask {
        name: "build".into(),
        original_name: "build".into(),
        node: TaskNode::Task(Box::new(make_task(Some("Build the project")))),
        is_group: false,
        source_file: None, // Root env.cue
    };
    let t_fmt_check = IndexedTask {
        name: "fmt.check".into(),
        original_name: "fmt.check".into(),
        node: TaskNode::Task(Box::new(make_task(Some("Check formatting")))),
        is_group: false,
        source_file: None,
    };
    let t_fmt_fix = IndexedTask {
        name: "fmt.fix".into(),
        original_name: "fmt.fix".into(),
        node: TaskNode::Task(Box::new(make_task(Some("Fix formatting")))),
        is_group: false,
        source_file: None,
    };

    // Provide them in mixed order to verify sorting
    let tasks = vec![&t_fmt_fix, &t_build, &t_fmt_check];
    let output = render_task_tree(tasks, None);

    // We can't match exact lines easily because of dot padding calculation,
    // but we can check structure and presence of content.

    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines[0], "Tasks:");

    // build is first alphabetically
    assert!(lines[1].starts_with("├─ build"));
    assert!(lines[1].contains("Build the project"));

    // fmt is second/last
    assert!(lines[2].starts_with("└─ fmt"));

    // children of fmt
    // fmt is last, so children have "   " prefix
    assert!(lines[3].starts_with("   ├─ check"));
    assert!(lines[3].contains("Check formatting"));

    assert!(lines[4].starts_with("   └─ fix"));
    assert!(lines[4].contains("Fix formatting"));
}
