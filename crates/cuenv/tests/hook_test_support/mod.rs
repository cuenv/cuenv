mod garbage;
mod multiline;
mod shell;

use assert_cmd::Command;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Output;
use tempfile::TempDir;

pub type TestResult<T = ()> = Result<T, Box<dyn Error>>;

pub fn create_test_dir(module: &str) -> TestResult<TempDir> {
    let temp_dir = tempfile::Builder::new().prefix("cuenv_test_").tempdir()?;
    let path = temp_dir.path();
    fs::create_dir_all(path.join("cue.mod"))?;
    fs::create_dir_all(path.join(".cuenv-state"))?;
    fs::write(
        path.join(".cuenv-state/session"),
        uuid::Uuid::new_v4().to_string(),
    )?;
    fs::write(
        path.join("cue.mod/module.cue"),
        format!("module: \"{module}\"\nlanguage: version: \"v0.9.0\"\n"),
    )?;
    Ok(temp_dir)
}

pub fn approve_config(path: &Path, cuenv_bin: &str) -> TestResult {
    let output = run_cuenv(path, cuenv_bin, &["allow", "--yes"])?;

    assert!(
        output.status.success(),
        "cuenv allow failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

pub fn load_hook_exports(path: &Path, cuenv_bin: &str) -> TestResult<Output> {
    let load = run_cuenv(path, cuenv_bin, &["env", "load"])?;
    assert_success(&load, "cuenv env load");

    let status = run_cuenv(
        path,
        cuenv_bin,
        &[
            "env",
            "status",
            "--wait",
            "--timeout",
            "10",
            "--output",
            "short",
        ],
    )?;
    assert_success(&status, "cuenv env status --wait");
    assert_eq!(
        String::from_utf8_lossy(&status.stdout).trim(),
        "[OK]",
        "hook execution did not complete successfully; stderr={}",
        String::from_utf8_lossy(&status.stderr)
    );

    let export = run_cuenv(path, cuenv_bin, &["export", "--shell", "bash"])?;
    assert_success(&export, "cuenv export");
    Ok(export)
}

pub fn run_cuenv(path: &Path, cuenv_bin: &str, args: &[&str]) -> TestResult<Output> {
    let session = fs::read_to_string(path.join(".cuenv-state/session"))?;
    Ok(Command::new(cuenv_bin)
        .current_dir(path)
        .env("CUENV_EXECUTABLE", cuenv_bin)
        .env("CUENV_STATE_DIR", state_dir(path))
        .env("CUENV_HOOK_SESSION", session)
        .env("CUENV_APPROVAL_FILE", approval_file(path))
        .args(args)
        .output()?)
}

fn assert_success(output: &Output, command: &str) {
    assert!(
        output.status.success(),
        "{command} failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn state_dir(path: &Path) -> std::path::PathBuf {
    path.join(".cuenv-state/state")
}

fn approval_file(path: &Path) -> std::path::PathBuf {
    path.join(".cuenv-state/approved.json")
}
