use assert_cmd::Command;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Output;
use tempfile::TempDir;

pub type TestResult<T = ()> = Result<T, Box<dyn Error>>;

pub enum ApprovalOutcome {
    Approved,
    SandboxError,
}

pub fn create_test_dir(module: &str) -> TestResult<TempDir> {
    let temp_dir = tempfile::Builder::new().prefix("cuenv_test_").tempdir()?;
    let path = temp_dir.path();
    fs::create_dir_all(path.join("cue.mod"))?;
    fs::write(
        path.join("cue.mod/module.cue"),
        format!("module: \"{module}\"\nlanguage: version: \"v0.9.0\"\n"),
    )?;
    Ok(temp_dir)
}

pub fn approve_config(path: &Path, cuenv_bin: &str) -> TestResult<ApprovalOutcome> {
    let output = Command::new(cuenv_bin)
        .current_dir(path)
        .env("CUENV_EXECUTABLE", cuenv_bin)
        .args(["allow", "--yes"])
        .output()?;

    if output.status.code() == Some(3) {
        assert_sandbox_error(&output, "during allow");
        return Ok(ApprovalOutcome::SandboxError);
    }

    assert!(
        output.status.success(),
        "cuenv allow failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(ApprovalOutcome::Approved)
}

pub fn assert_sandbox_error(output: &Output, context: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Evaluation/FFI error") || stderr.contains("Unexpected error"),
        "Expected FFI or Unexpected error {context}, got: {stderr}"
    );
}
