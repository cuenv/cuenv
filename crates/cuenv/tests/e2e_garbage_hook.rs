//! Integration test for hooks with syntax errors

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
    cmd.current_dir(path)
        .env("CUENV_EXECUTABLE", cuenv_bin)
        .arg("allow")
        .assert()
        .success();

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

    // Handle different behaviors in sandbox vs local environment
    if output.status.code() == Some(3) {
        // CI / Sandbox behavior: Abort with error (could be FFI or I/O)
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Evaluation/FFI error") || stderr.contains("Unexpected error"),
            "Expected FFI or I/O error in stderr, got: {stderr}"
        );
    } else {
        // Local / Permissive behavior: Continue with partial env
        assert_eq!(
            output.status.code(),
            Some(1),
            "Expected exit code 1 (MISSING)"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("MISSING"),
            "Expected MISSING in stdout, got: {stdout}"
        );
    }
}
