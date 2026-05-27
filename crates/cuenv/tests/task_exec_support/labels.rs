use super::{TestResult, create_test_dir, init_cue_module, run_cuenv};
use std::fs;
use std::io;
use std::path::Path;
use tempfile::TempDir;

fn write_env(temp_dir: &TempDir, cue_content: &str) -> TestResult {
    fs::write(temp_dir.path().join("env.cue"), cue_content)?;
    Ok(())
}

fn write_project_env(project_dir: &Path, cue_content: &str) -> TestResult {
    fs::create_dir_all(project_dir)?;
    fs::write(project_dir.join("env.cue"), cue_content)?;
    Ok(())
}

fn path_arg(path: &Path) -> TestResult<&str> {
    path.to_str()
        .ok_or_else(|| io::Error::other(format!("path is not valid UTF-8: {}", path.display())))
        .map_err(Into::into)
}

#[test]
fn test_task_label_execution_is_path_scoped() -> TestResult {
    let temp_dir = create_test_dir()?;
    init_cue_module(temp_dir.path())?;

    // All projects must use `package cuenv` - this is enforced by cuenv
    let project_a = temp_dir.path().join("project-a");
    write_project_env(
        &project_a,
        r#"package cuenv

name: "project-a"

env: {}

tasks: {
  projen: {
    command: "sh"
    args: ["-c", "echo A-PROJEN"]
    labels: ["projen"]
  }
}
"#,
    )?;

    let project_b = temp_dir.path().join("project-b");
    write_project_env(
        &project_b,
        r#"package cuenv

name: "project-b"

env: {}

tasks: {
  generate: {
    command: "sh"
    args: ["-c", "echo B-PROJEN"]
    labels: ["projen"]
  }
}
"#,
    )?;

    let (stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        path_arg(&project_a)?,
        "--package",
        "cuenv",
        "-l",
        "projen",
    ])?;

    assert!(
        success,
        "Expected success.\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("A-PROJEN"));
    assert!(
        !stdout.contains("B-PROJEN"),
        "Label execution must be scoped to the selected path"
    );
    Ok(())
}

#[test]
fn test_task_label_multiple_labels_and_semantics() -> TestResult {
    let temp_dir = create_test_dir()?;
    init_cue_module(temp_dir.path())?;

    // Create a project with multiple tasks having different label combinations
    write_env(
        &temp_dir,
        r#"package test

name: "test"

env: {}

tasks: {
  unit_tests: {
    command: "sh"
    args: ["-c", "echo UNIT-TESTS"]
    labels: ["test", "unit"]
  }
  e2e_tests: {
    command: "sh"
    args: ["-c", "echo E2E-TESTS"]
    labels: ["test"]
  }
  build: {
    command: "sh"
    args: ["-c", "echo BUILD"]
    labels: ["build"]
  }
}
"#,
    )?;

    // Test: Multiple labels with AND semantics - only unit_tests has both "test" AND "unit"
    let (stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        path_arg(temp_dir.path())?,
        "--package",
        "test",
        "-l",
        "test",
        "-l",
        "unit",
    ])?;

    assert!(
        success,
        "Expected success.\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    // Only unit_tests should match (has both "test" and "unit" labels)
    assert!(
        stdout.contains("UNIT-TESTS"),
        "Should execute unit_tests (has both labels)"
    );
    // e2e_tests only has "test" label, not "unit", so it shouldn't match
    assert!(
        !stdout.contains("E2E-TESTS"),
        "Should NOT execute e2e_tests (missing 'unit' label)"
    );
    assert!(
        !stdout.contains("BUILD"),
        "Should NOT execute build (has neither label)"
    );
    Ok(())
}

#[test]
fn test_task_label_error_conflicting_task_name_and_label() -> TestResult {
    let temp_dir = create_test_dir()?;
    init_cue_module(temp_dir.path())?;
    write_env(
        &temp_dir,
        r#"package test
name: "test"
env: {}
tasks: {
  mytask: {
    command: "echo"
    args: ["hello"]
    labels: ["test"]
  }
}
"#,
    )?;

    // Test: Cannot specify both task name and --label
    let (_stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        path_arg(temp_dir.path())?,
        "--package",
        "test",
        "mytask",
        "-l",
        "test",
    ])?;

    assert!(
        !success,
        "Expected failure when specifying both task name and label"
    );
    // Note: miette may line-wrap the message, so check for key parts separately
    assert!(
        stderr.contains("Cannot specify both a task name") && stderr.contains("--label"),
        "Error message should mention conflict. Got: {stderr}"
    );
    Ok(())
}

#[test]
fn test_task_label_error_trailing_args_become_task_name() -> TestResult {
    let temp_dir = create_test_dir()?;
    init_cue_module(temp_dir.path())?;
    write_env(
        &temp_dir,
        r#"package test
name: "test"
env: {}
tasks: {
  mytask: {
    command: "echo"
    labels: ["test"]
  }
}
"#,
    )?;

    // Test: Trailing args after -- are interpreted as task name (first positional)
    // Since task name conflicts with --label, we get the conflict error
    let (_stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        path_arg(temp_dir.path())?,
        "--package",
        "test",
        "-l",
        "test",
        "--",
        "arg1",
        "arg2",
    ])?;

    assert!(
        !success,
        "Expected failure when using trailing args with label selection"
    );
    // With trailing_var_arg, "arg1" becomes the task name, triggering the conflict error
    // Note: miette may line-wrap the message, so check for key parts separately
    assert!(
        stderr.contains("Cannot specify both a task name") && stderr.contains("--label"),
        "Error message should mention conflict (trailing arg becomes task name). Got: {stderr}"
    );
    Ok(())
}

#[test]
fn test_task_label_error_no_matching_tasks() -> TestResult {
    let temp_dir = create_test_dir()?;
    init_cue_module(temp_dir.path())?;
    write_env(
        &temp_dir,
        r#"package test
name: "test"
env: {}
tasks: {
  mytask: {
    command: "echo"
    args: ["hello"]
    labels: ["existing"]
  }
}
"#,
    )?;

    // Test: No tasks match the given label
    let (_stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        path_arg(temp_dir.path())?,
        "--package",
        "test",
        "-l",
        "nonexistent",
    ])?;

    assert!(!success, "Expected failure when no tasks match label");
    assert!(
        stderr.contains("No tasks with labels") && stderr.contains("nonexistent"),
        "Error message should mention no matching tasks. Got: {stderr}"
    );
    Ok(())
}

#[test]
fn test_task_label_error_empty_labels() -> TestResult {
    let temp_dir = create_test_dir()?;
    init_cue_module(temp_dir.path())?;
    write_env(
        &temp_dir,
        r#"package test
name: "test"
env: {}
tasks: {
  mytask: {
    command: "echo"
    labels: ["test"]
  }
}
"#,
    )?;

    // Test: Empty/whitespace-only labels should error
    let (_stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        path_arg(temp_dir.path())?,
        "--package",
        "test",
        "-l",
        "   ",
    ])?;

    assert!(!success, "Expected failure with empty/whitespace labels");
    assert!(
        stderr.contains("empty") || stderr.contains("whitespace"),
        "Error message should mention empty/whitespace labels. Got: {stderr}"
    );
    Ok(())
}
