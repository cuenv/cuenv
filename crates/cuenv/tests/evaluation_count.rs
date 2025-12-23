//! Regression tests for CUE module evaluation count.
//!
//! These tests ensure that `sync -A` and related commands only evaluate
//! the CUE module once, using the `CommandExecutor`'s cached module.
//!
//! Note: These tests run against the actual cuenv repository since setting up
//! a proper CUE environment with schema imports in a temp directory is complex.

use std::process::Command;

/// Run cuenv with debug logging from the project root and count module evaluations.
fn count_evaluate_module_calls_in_repo(args: &[&str]) -> usize {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let project_root = std::path::Path::new(manifest_dir)
        .parent() // crates
        .and_then(|p| p.parent()) // project root
        .expect("Failed to find project root");

    let mut cmd = Command::new("cargo");
    cmd.arg("run")
        .arg("--bin")
        .arg("cuenv")
        .arg("--")
        .arg("-L")
        .arg("debug");

    for arg in args {
        cmd.arg(arg);
    }

    cmd.current_dir(project_root);

    let output = cmd.output().expect("Failed to run cuenv");
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Count occurrences of the module evaluation log message
    // This message is logged at INFO level in cuengine::evaluate_module
    stderr
        .matches("Starting module-wide CUE evaluation")
        .count()
}

/// Test that `sync -A` evaluates the CUE module exactly once.
///
/// This is a regression test for the performance issue where each sync provider
/// was independently evaluating the module instead of sharing the cached result.
#[test]
fn test_sync_all_evaluates_module_once() {
    let eval_count = count_evaluate_module_calls_in_repo(&["sync", "-A"]);

    // Should evaluate exactly once - the CommandExecutor caches the result
    assert_eq!(
        eval_count, 1,
        "sync -A should evaluate the CUE module exactly once, but it was evaluated {eval_count} times. \
         This indicates a regression in module caching."
    );
}

/// Test that `sync codeowners` evaluates the CUE module exactly once.
#[test]
fn test_sync_codeowners_evaluates_module_once() {
    let eval_count = count_evaluate_module_calls_in_repo(&["sync", "codeowners"]);

    assert_eq!(
        eval_count, 1,
        "sync codeowners should evaluate the CUE module exactly once, but it was evaluated {eval_count} times."
    );
}

/// Test that `sync ignore` evaluates the CUE module exactly once.
#[test]
fn test_sync_ignore_evaluates_module_once() {
    let eval_count = count_evaluate_module_calls_in_repo(&["sync", "ignore"]);

    assert_eq!(
        eval_count, 1,
        "sync ignore should evaluate the CUE module exactly once, but it was evaluated {eval_count} times."
    );
}
