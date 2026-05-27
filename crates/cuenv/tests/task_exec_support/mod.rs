#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

mod basic;
mod examples;
mod hermetic;
mod labels;

use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

/// Create a Command with a clean environment (no CI vars leaking).
fn clean_environment_command(bin: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("USER", std::env::var("USER").unwrap_or_default());
    cmd
}

/// Create a test directory with proper prefix (non-hidden) for CUE loader compatibility.
///
/// CUE's `load.Instances` ignores directories starting with `.` (hidden directories).
/// The default `TempDir::new()` creates hidden directories like `.tmpXXXXX`, which causes
/// CUE evaluation to fail with "No instances could be evaluated".
fn create_test_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("cuenv_test_")
        .tempdir()
        .expect("Failed to create temp directory")
}

/// Initialize a CUE module in the given directory.
fn init_cue_module(dir: &Path) {
    fs::create_dir_all(dir.join("cue.mod")).unwrap();
    fs::write(
        dir.join("cue.mod/module.cue"),
        r#"module: "test.example/test"
language: version: "v0.9.0"
"#,
    )
    .unwrap();
}

/// Find a binary by name in the current PATH, returning its absolute path.
/// This avoids hardcoding paths like `/usr/bin/printenv` which don't exist in Nix sandboxes.
fn find_in_path(name: &str) -> String {
    let path_var = std::env::var("PATH").unwrap_or_default();
    for dir in path_var.split(':') {
        let candidate = std::path::PathBuf::from(dir).join(name);
        if candidate.is_file() {
            return candidate.to_string_lossy().to_string();
        }
    }
    panic!("Could not find `{name}` in PATH");
}

/// Helper to run cuenv command and capture output
fn run_cuenv(args: &[&str]) -> (String, String, bool) {
    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");
    let output = clean_environment_command(cuenv_bin)
        .args(args)
        .output()
        .expect("Failed to run cuenv");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let success = output.status.success();

    (stdout, stderr, success)
}
