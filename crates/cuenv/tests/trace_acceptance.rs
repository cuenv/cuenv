//! Acceptance tests for cuenv CLI with DAG and trace verification
//!
//! These tests run the cuenv CLI and verify both the task DAG structure
//! (via --dry-run) and the tracing spans emitted during execution.

mod trace_testing;

use serde::Deserialize;
use std::error::Error;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// Create a Command with a clean environment (no CI vars leaking).
/// This prevents tests from hanging when run in CI environments where
/// variables like GITHUB_ACTIONS=true would trigger CI-specific code paths.
fn clean_environment_command(bin: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("USER", std::env::var("USER").unwrap_or_default());
    cmd
}

/// DAG export structure matching the --dry-run output
#[derive(Debug, Deserialize)]
struct DagExport {
    tasks: Vec<DagTask>,
    execution_order: Vec<String>,
    parallel_groups: Vec<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct DagTask {
    name: String,
    dependencies: Vec<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    description: Option<String>,
}

/// Get path to the cuenv binary
fn get_cuenv_bin() -> TestResult<PathBuf> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Ok(Path::new(manifest_dir)
        .parent() // crates
        .and_then(|p| p.parent()) // project root
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "failed to find project root"))?
        .join("target")
        .join("debug")
        .join("cuenv"))
}

/// Get path to examples directory
fn get_examples_dir() -> TestResult<PathBuf> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Ok(Path::new(manifest_dir)
        .parent() // crates
        .and_then(|p| p.parent()) // project root
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "failed to find project root"))?
        .join("examples"))
}

/// Check if the cuenv binary exists
fn binary_available() -> bool {
    get_cuenv_bin().is_ok_and(|bin| bin.exists())
}

/// Skip test if binary is not available
macro_rules! skip_if_binary_unavailable {
    () => {
        if !binary_available() {
            return Ok(());
        }
    };
}

/// Run cuenv with --dry-run and parse the DAG export
fn run_dry_run(example: &str, task: &str) -> TestResult<DagExport> {
    let bin = get_cuenv_bin()?;
    let examples_dir = get_examples_dir()?;
    let example_path = examples_dir.join(example);

    let output = clean_environment_command(&bin)
        .args(["task", task, "--dry-run", "--path"])
        .arg(&example_path)
        .args(["--package", "examples"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Box::new(io::Error::other(format!(
            "cuenv --dry-run failed: {stderr}"
        ))));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).map_err(|e| {
        Box::new(io::Error::other(format!(
            "Failed to parse DAG JSON: {e}\nOutput: {stdout}"
        ))) as Box<dyn Error>
    })
}

// =============================================================================
// DAG Structure Tests
// =============================================================================

#[test]
fn test_task_basic_greetall_dag_structure() -> TestResult {
    skip_if_binary_unavailable!();

    let dag = run_dry_run("task-basic", "greetAll")?;

    // Verify we got some tasks in the DAG
    assert!(!dag.tasks.is_empty(), "DAG should have tasks");

    // Verify we have parallel groups
    assert!(
        !dag.parallel_groups.is_empty(),
        "DAG should have parallel groups"
    );

    // Verify execution order is consistent with tasks
    for task in &dag.tasks {
        assert!(
            dag.execution_order.contains(&task.name),
            "Task {} should be in execution order",
            task.name
        );
    }

    Ok(())
}

#[test]
fn test_task_basic_parallel_groups() -> TestResult {
    skip_if_binary_unavailable!();

    let dag = run_dry_run("task-basic", "greetAll")?;

    // Verify parallel groups exist
    assert!(
        !dag.parallel_groups.is_empty(),
        "Should have at least one parallel group"
    );

    // First group should contain tasks with no dependencies
    let first_group = &dag.parallel_groups[0];
    for task_name in first_group {
        let task = dag
            .tasks
            .iter()
            .find(|t| &t.name == task_name)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("task {task_name} in parallel group should exist"),
                )
            })?;
        assert!(
            task.dependencies.is_empty(),
            "Tasks in first parallel group should have no dependencies"
        );
    }

    Ok(())
}

#[test]
fn test_interpolate_task_dag() -> TestResult {
    skip_if_binary_unavailable!();

    let dag = run_dry_run("task-basic", "interpolate")?;

    // Verify DAG structure
    assert!(!dag.tasks.is_empty(), "DAG should have tasks");
    assert!(
        !dag.parallel_groups.is_empty(),
        "Should have parallel groups"
    );

    // All tasks should be in execution order
    for task in &dag.tasks {
        assert!(
            dag.execution_order.contains(&task.name),
            "Task {} should be in execution order",
            task.name
        );
    }

    Ok(())
}

#[test]
fn test_dag_export_includes_command_info() -> TestResult {
    skip_if_binary_unavailable!();

    let dag = run_dry_run("task-basic", "interpolate")?;

    // All tasks should have command info
    for task in &dag.tasks {
        assert!(
            task.command.is_some(),
            "Task {} should have command information",
            task.name
        );
    }

    Ok(())
}

// =============================================================================
// Hook Example DAG Tests
// =============================================================================

#[test]
fn test_hook_example_verify_env_dag() -> TestResult {
    skip_if_binary_unavailable!();

    let dag = run_dry_run("hook", "verify_env")?;

    // DAG should have tasks
    assert!(!dag.tasks.is_empty(), "DAG should have tasks");

    // Verify execution order is populated
    assert!(
        !dag.execution_order.is_empty(),
        "Execution order should be populated"
    );

    Ok(())
}

// =============================================================================
// Dagger Task Example DAG Tests
// =============================================================================

#[test]
fn test_dagger_task_hello_dag() -> TestResult {
    skip_if_binary_unavailable!();

    let dag = run_dry_run("dagger-task", "hello")?;

    // DAG should have tasks
    assert!(!dag.tasks.is_empty(), "DAG should have tasks");

    // All tasks should have commands
    for task in &dag.tasks {
        assert!(
            task.command.is_some(),
            "Task {} should have a command",
            task.name
        );
    }

    Ok(())
}

// =============================================================================
// CI Pipeline Example Tests
// =============================================================================

#[test]
fn test_ci_pipeline_test_task_dag() -> TestResult {
    skip_if_binary_unavailable!();

    let dag = run_dry_run("ci-pipeline", "test")?;

    // DAG should have tasks
    assert!(!dag.tasks.is_empty(), "DAG should have tasks");

    // All tasks should have commands
    for task in &dag.tasks {
        assert!(
            task.command.is_some(),
            "Task {} should have a command",
            task.name
        );
    }

    Ok(())
}

// =============================================================================
// Edge Cases
// =============================================================================

#[test]
fn test_dry_run_with_nonexistent_task_fails() -> TestResult {
    skip_if_binary_unavailable!();

    let result = run_dry_run("task-basic", "nonexistent_task_12345");

    assert!(result.is_err(), "dry-run with nonexistent task should fail");

    Ok(())
}

#[test]
fn test_dry_run_with_nonexistent_example_fails() -> TestResult {
    skip_if_binary_unavailable!();

    let bin = get_cuenv_bin()?;
    let examples_dir = get_examples_dir()?;
    let example_path = examples_dir.join("nonexistent-example-12345");

    let output = clean_environment_command(&bin)
        .args(["task", "test", "--dry-run", "--path"])
        .arg(&example_path)
        .args(["--package", "examples"])
        .output()?;

    assert!(
        !output.status.success(),
        "dry-run with nonexistent example should fail"
    );

    Ok(())
}
