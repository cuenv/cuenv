//! Integration test for hooks with multiline exports

mod hook_test_support;

use assert_cmd::Command;
use hook_test_support::{
    ApprovalOutcome, TestResult, approve_config, assert_sandbox_error, create_test_dir,
};
use std::fs;

#[test]
fn test_hook_multiline_export() -> TestResult {
    let temp_dir = create_test_dir("test.example/multiline")?;
    let path = temp_dir.path();

    // Create env.cue with a hook that exports a multiline variable
    // We also export SINGLE_LINE to see if *that* gets lost too if the script fails
    let cue_content = r#"
package cuenv

name: "test"

hooks: {
    onEnter: {
        multiline_hook: {
            command: "sh"
            args: ["-c", "echo 'export MULTI=\"line1\nline2\"'; echo 'export SINGLE=success'"]
            source: true
        }
    }
}
"#;
    fs::write(path.join("env.cue"), cue_content)?;

    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");

    match approve_config(path, cuenv_bin)? {
        ApprovalOutcome::Approved => {}
        ApprovalOutcome::SandboxError => return Ok(()),
    }

    // Check SINGLE variable first - if multiline broke the script, this will likely be missing too
    let output = Command::new(cuenv_bin)
        .current_dir(path)
        .env("CUENV_EXECUTABLE", cuenv_bin)
        .args([
            "exec",
            "--",
            "sh",
            "-c",
            "if [ \"$SINGLE\" = \"success\" ]; then echo FOUND_SINGLE; else echo MISSING_SINGLE; fi; if [ \"$MULTI\" = \"line1\nline2\" ]; then echo FOUND_MULTI; else echo MISSING_MULTI; fi",
        ])
        .output()?;

    // If the bug is fixed, both should be found
    // Handle FFI error in sandbox
    if output.status.code() == Some(3) {
        assert_sandbox_error(&output, "in sandbox");
    } else {
        assert!(
            output.status.success(),
            "cuenv exec failed: stdout={}, stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("FOUND_MULTI"),
            "Expected FOUND_MULTI in stdout, got: {stdout}"
        );
    }
    Ok(())
}
