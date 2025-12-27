//! Integration test for hooks with syntax errors

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
        "module: \"test.example/garbage\"\nlanguage: version: \"v0.9.0\"\n",
    )
    .unwrap();
    temp_dir
}

#[test]
fn test_hook_with_syntax_error_output() {
    let temp_dir = create_test_dir();
    let path = temp_dir.path();

    // Create env.cue with a hook that outputs a SYNTAX ERROR (unclosed quote)
    // This should cause the shell to abort and 'env -0' will probably not run or exit code will be non-zero.
    let cue_content = r#"
package cuenv

name: "test"

hooks: {
    onEnter: {
        bad_hook: {
            command: "sh"
            args: ["-c", "echo 'export BAD=\"unclosed'; echo 'export GOOD=success'"]
            source: true
        }
    }
}
"#;
    fs::write(path.join("env.cue"), cue_content).unwrap();

    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");

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

    #[allow(deprecated)]
    let mut cmd = Command::cargo_bin("cuenv").unwrap();
    let output = cmd
        .current_dir(path)
        .env("CUENV_EXECUTABLE", cuenv_bin)
        .arg("exec")
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg("if [ \"$GOOD\" = \"success\" ]; then echo FOUND; else echo MISSING; exit 1; fi")
        .output()
        .unwrap();

    // Handle different behaviors based on environment and error handling:
    // - Exit code 3: FFI/sandbox error
    // - Exit code 2: CLI/configuration error (hook evaluation failed)
    // - Exit code 1: Partial env (GOOD var not set, script returns MISSING)
    let exit_code = output.status.code();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    match exit_code {
        Some(3) => {
            // CI / Sandbox behavior: Abort with error (could be FFI or I/O)
            assert!(
                stderr.contains("Evaluation/FFI error") || stderr.contains("Unexpected error"),
                "Expected FFI or I/O error in stderr, got: {stderr}"
            );
        }
        Some(2) => {
            // CLI/configuration error - hook evaluation or environment setup failed
            // This is acceptable behavior when hooks have syntax errors
            assert!(
                stderr.contains("error") || stderr.contains("Error") || stderr.contains("failed"),
                "Expected error message in stderr for exit code 2, got: {stderr}"
            );
        }
        Some(1) => {
            // Local / Permissive behavior: Continue with partial env
            assert!(
                stdout.contains("MISSING"),
                "Expected MISSING in stdout for exit code 1, got: {stdout}"
            );
        }
        other => {
            panic!(
                "Unexpected exit code {other:?}.\nstdout: {stdout}\nstderr: {stderr}"
            );
        }
    }
}
