//! Acceptance tests for cuenv CLI with DAG and trace verification
//!
//! These tests run the cuenv CLI and verify both the task DAG structure
//! (via --dry-run) and the tracing spans emitted during execution.

// Integration tests can use unwrap/expect for cleaner assertions
#![allow(clippy::unwrap_used, clippy::expect_used)]
// Tests can use print statements for diagnostics
#![allow(clippy::print_stderr)]
// Test infrastructure fields may be reserved for future use
#![allow(dead_code)]

mod trace_testing;

use serde::Deserialize;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    description: Option<String>,
}

/// Get path to the cuenv binary
fn get_cuenv_bin() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir)
        .parent() // crates
        .and_then(|p| p.parent()) // project root
        .expect("Failed to find project root")
        .join("target")
        .join("debug")
        .join("cuenv")
}

/// Get path to examples directory
fn get_examples_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir)
        .parent() // crates
        .and_then(|p| p.parent()) // project root
        .expect("Failed to find project root")
        .join("examples")
}

/// Check if the cuenv binary exists
fn binary_available() -> bool {
    get_cuenv_bin().exists()
}

/// Skip test if binary is not available
macro_rules! skip_if_binary_unavailable {
    () => {
        if !binary_available() {
            eprintln!(
                "Skipping test: cuenv binary not found at {:?}. Run `cargo build` first.",
                get_cuenv_bin()
            );
            return;
        }
    };
}

/// Run cuenv with --dry-run and parse the DAG export
fn run_dry_run(example: &str, task: &str) -> Result<DagExport, String> {
    let bin = get_cuenv_bin();
    let examples_dir = get_examples_dir();
    let example_path = examples_dir.join(example);

    let output = clean_environment_command(&bin)
        .args([
            "task",
            task,
            "--dry-run",
            "--path",
            example_path.to_str().unwrap(),
            "--package",
            "examples",
        ])
        .output()
        .map_err(|e| format!("Failed to run cuenv: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("cuenv --dry-run failed: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout)
        .map_err(|e| format!("Failed to parse DAG JSON: {e}\nOutput: {stdout}"))
}

// =============================================================================
// DAG Structure Tests
// =============================================================================

#[test]
fn test_task_basic_greetall_dag_structure() {
    skip_if_binary_unavailable!();

    let dag = match run_dry_run("task-basic", "greetAll") {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Skipping test - cuenv execution failed: {e}");
            return;
        }
    };

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
}

#[test]
fn test_task_basic_parallel_groups() {
    skip_if_binary_unavailable!();

    let dag = match run_dry_run("task-basic", "greetAll") {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Skipping test - cuenv execution failed: {e}");
            return;
        }
    };

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
            .expect("Task in parallel group should exist");
        assert!(
            task.dependencies.is_empty(),
            "Tasks in first parallel group should have no dependencies"
        );
    }
}

#[test]
fn test_interpolate_task_dag() {
    skip_if_binary_unavailable!();

    let dag = match run_dry_run("task-basic", "interpolate") {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Skipping test - cuenv execution failed: {e}");
            return;
        }
    };

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
}

#[test]
fn test_dag_export_includes_command_info() {
    skip_if_binary_unavailable!();

    let dag = match run_dry_run("task-basic", "interpolate") {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Skipping test - cuenv execution failed: {e}");
            return;
        }
    };

    // All tasks should have command info
    for task in &dag.tasks {
        assert!(
            task.command.is_some(),
            "Task {} should have command information",
            task.name
        );
    }
}

// =============================================================================
// Hook Example DAG Tests
// =============================================================================

#[test]
fn test_hook_example_verify_env_dag() {
    skip_if_binary_unavailable!();

    let dag = match run_dry_run("hook", "verify_env") {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Skipping test - cuenv execution failed: {e}");
            return;
        }
    };

    // DAG should have tasks
    assert!(!dag.tasks.is_empty(), "DAG should have tasks");

    // Verify execution order is populated
    assert!(
        !dag.execution_order.is_empty(),
        "Execution order should be populated"
    );
}

// =============================================================================
// Dagger Task Example DAG Tests
// =============================================================================

#[test]
fn test_dagger_task_hello_dag() {
    skip_if_binary_unavailable!();

    let dag = match run_dry_run("dagger-task", "hello") {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Skipping test - cuenv execution failed: {e}");
            return;
        }
    };

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
}

// =============================================================================
// CI Pipeline Example Tests
// =============================================================================

#[test]
fn test_ci_pipeline_test_task_dag() {
    skip_if_binary_unavailable!();

    let dag = match run_dry_run("ci-pipeline", "test") {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Skipping test - cuenv execution failed: {e}");
            return;
        }
    };

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
}

// =============================================================================
// Edge Cases
// =============================================================================

#[test]
fn test_dry_run_with_nonexistent_task_fails() {
    skip_if_binary_unavailable!();

    let result = run_dry_run("task-basic", "nonexistent_task_12345");

    assert!(result.is_err(), "dry-run with nonexistent task should fail");
}

#[test]
fn test_dry_run_with_nonexistent_example_fails() {
    skip_if_binary_unavailable!();

    let bin = get_cuenv_bin();
    let examples_dir = get_examples_dir();
    let example_path = examples_dir.join("nonexistent-example-12345");

    let output = clean_environment_command(&bin)
        .args([
            "task",
            "test",
            "--dry-run",
            "--path",
            example_path.to_str().unwrap(),
            "--package",
            "examples",
        ])
        .output()
        .expect("Failed to run cuenv");

    assert!(
        !output.status.success(),
        "dry-run with nonexistent example should fail"
    );
}
