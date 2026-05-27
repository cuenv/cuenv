//! Regression tests for CUE module evaluation count.
//!
//! These tests verify evaluation scope behavior:
//! - path sync evaluates only local scope
//! - workspace sync (`-A`) evaluates workspace scope
//!
//! Note: These tests run against the actual cuenv repository since setting up
//! a proper CUE environment with schema imports in a temp directory is complex.

use std::error::Error;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

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
fn count_evaluate_module_calls_in_repo(args: &[&str]) -> TestResult<usize> {
    let project_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates
        .and_then(|p| p.parent()) // project root
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "failed to find project root")
        })?;

    let mut cmd = clean_environment_command(env!("CARGO_BIN_EXE_cuenv"));
    cmd.arg("-L").arg("debug");
    cmd.args(args).current_dir(project_root);

    let output = cmd.output()?;
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Count occurrences of the module evaluation log message
    // This message is logged at INFO level in cuengine::evaluate_module
    Ok(stderr
        .matches("Starting module-wide CUE evaluation")
        .count())
}

/// Test that workspace sync (`-A`) evaluates more than one module in this repo.
#[test]
fn test_sync_all_evaluates_workspace_scope() -> TestResult {
    let eval_count = count_evaluate_module_calls_in_repo(&["sync", "-A"])?;
    assert!(
        eval_count > 1,
        "sync -A should evaluate workspace scope (multiple modules) in this repository, but evaluated {eval_count}"
    );
    Ok(())
}

/// Test that path sync stays path-local.
#[test]
fn test_sync_path_evaluates_local_scope() -> TestResult {
    let eval_count = count_evaluate_module_calls_in_repo(&["sync"])?;
    assert!(
        eval_count <= 2,
        "sync (without -A) should stay path-local and avoid workspace fan-out, but evaluated {eval_count} modules"
    );
    Ok(())
}
