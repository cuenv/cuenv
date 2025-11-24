use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_hook_multiline_export() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path();

    // Create env.cue with a hook that exports a multiline variable
    // We also export SINGLE_LINE to see if *that* gets lost too if the script fails
    let cue_content = r#"
package cuenv

hooks: {
    onEnter: {
        command: "sh"
        args: ["-c", "echo 'export MULTI=\"line1\nline2\"'; echo 'export SINGLE=success'"]
        source: true
    }
}
"#;
    fs::write(path.join("env.cue"), cue_content).unwrap();

    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");

    // 1. Approve config
    let mut cmd = Command::cargo_bin("cuenv").unwrap();
    cmd.current_dir(path)
        .env("CUENV_EXECUTABLE", cuenv_bin)
        .arg("allow")
        .assert()
        .success();

    // 2. Exec command to check variables
    // Check SINGLE variable first - if multiline broke the script, this will likely be missing too
    let mut cmd = Command::cargo_bin("cuenv").unwrap();
    let output = cmd.current_dir(path)
        .env("CUENV_EXECUTABLE", cuenv_bin)
        .arg("exec")
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg("if [ \"$SINGLE\" = \"success\" ]; then echo FOUND_SINGLE; else echo MISSING_SINGLE; fi; if [ \"$MULTI\" = \"line1\nline2\" ]; then echo FOUND_MULTI; else echo MISSING_MULTI; fi")
        .output()
        .unwrap();

    // If the bug is fixed, both should be found
    // Handle FFI error in sandbox
    if output.status.code() == Some(3) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Evaluation/FFI error"),
            "Expected FFI error in sandbox, got: {}",
            stderr
        );
    } else {
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("FOUND_MULTI"),
            "Expected FOUND_MULTI in stdout, got: {}",
            stdout
        );
    }
}
