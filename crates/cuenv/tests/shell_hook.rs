//! Integration test for onEnter hooks that include shellHook-style logic

// Integration tests can use unwrap/expect for cleaner assertions
#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

/// Create a test directory with non-hidden name and CUE module setup
fn create_test_dir() -> TempDir {
    let temp_dir = tempfile::Builder::new()
        .prefix("cuenv_test_")
        .tempdir()
        .expect("Failed to create temp directory");
    let path = temp_dir.path();
    // Create CUE module for module-wide evaluation
    fs::create_dir_all(path.join("cue.mod")).unwrap();
    fs::write(
        path.join("cue.mod/module.cue"),
        "module: \"test.example/shell-hook\"\nlanguage: version: \"v0.9.0\"\n",
    )
    .unwrap();
    temp_dir
}

#[test]
fn test_on_enter_shell_hook_exports() {
    let temp_dir = create_test_dir();
    let path = temp_dir.path();

    // Create env.cue with a hook that emits a shellHook function and invokes it
    let cue_content = r#"
package cuenv

name: "test"

hooks: {
    onEnter: {
        shell_hook: {
            command: "sh"
            args: ["-c", "printf '%s\\n' 'export BASE=ok' 'shellHook() { export SHELL_HOOK_VAR=from_shell_hook; }' 'shellHook'"]
            source: true
        }
    }
}
"#;
    fs::write(path.join("env.cue"), cue_content).unwrap();

    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");

    // 1. Approve config
    #[allow(deprecated)]
    let mut cmd = Command::cargo_bin("cuenv").unwrap();
    let allow_output = cmd
        .current_dir(path)
        .env("CUENV_EXECUTABLE", cuenv_bin)
        .arg("allow")
        .arg("--yes")
        .output()
        .unwrap();

    // Handle FFI error in sandbox during allow
    if allow_output.status.code() == Some(3) {
        let stderr = String::from_utf8_lossy(&allow_output.stderr);
        assert!(
            stderr.contains("Evaluation/FFI error") || stderr.contains("Unexpected error"),
            "Expected FFI or Unexpected error in sandbox during allow, got: {stderr}"
        );
        return; // Skip rest of test in sandbox
    }
    assert!(
        allow_output.status.success(),
        "cuenv allow failed: {}",
        String::from_utf8_lossy(&allow_output.stderr)
    );

    // 2. Exec command to check variables from shellHook
    #[allow(deprecated)]
    let mut cmd = Command::cargo_bin("cuenv").unwrap();
    let output = cmd
        .current_dir(path)
        .env("CUENV_EXECUTABLE", cuenv_bin)
        .arg("exec")
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg(
            "if [ \"$SHELL_HOOK_VAR\" = \"from_shell_hook\" ]; then echo FOUND; else echo MISSING; exit 1; fi",
        )
        .output()
        .unwrap();

    // Handle FFI error in sandbox
    if output.status.code() == Some(3) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Evaluation/FFI error") || stderr.contains("Unexpected error"),
            "Expected FFI or Unexpected error in sandbox, got: {stderr}"
        );
    } else {
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("FOUND"),
            "Expected FOUND in stdout, got: {stdout}"
        );
    }
}
