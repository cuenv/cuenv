//! Regression tests for CUE module evaluation count.
//!
//! These tests verify evaluation scope behavior:
//! - path sync evaluates only local scope
//! - workspace sync (`-A`) evaluates workspace scope
//!
//! Note: These tests run against the actual cuenv repository since setting up
//! a proper CUE environment with schema imports in a temp directory is complex.

// Integration tests can use unwrap/expect for cleaner assertions
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsStr;
use std::process::Command;

/// Create a Command with a clean environment (no CI vars leaking).
fn clean_environment_command(bin: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("USER", std::env::var("USER").unwrap_or_default());
    cmd
}

/// Run cuenv with debug logging from the project root and count module evaluations.
fn count_evaluate_module_calls_in_repo(args: &[&str]) -> usize {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let project_root = std::path::Path::new(manifest_dir)
        .parent() // crates
        .and_then(|p| p.parent()) // project root
        .expect("Failed to find project root");

    let mut cmd = clean_environment_command(env!("CARGO_BIN_EXE_cuenv"));
    cmd.arg("-L").arg("debug");

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

/// Test that workspace sync (`-A`) evaluates more than one module in this repo.
#[test]
fn test_sync_all_evaluates_workspace_scope() {
    let eval_count = count_evaluate_module_calls_in_repo(&["sync", "-A"]);
    assert!(
        eval_count > 1,
        "sync -A should evaluate workspace scope (multiple modules) in this repository, but evaluated {eval_count}"
    );
}

/// Test that path sync stays path-local.
#[test]
fn test_sync_path_evaluates_local_scope() {
    let eval_count = count_evaluate_module_calls_in_repo(&["sync"]);
    assert!(
        eval_count <= 2,
        "sync (without -A) should stay path-local and avoid workspace fan-out, but evaluated {eval_count} modules"
    );
}
