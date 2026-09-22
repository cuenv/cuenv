//! Integration test for hooks with multiline exports

mod hook_test_support;

use assert_cmd::Command;
use hook_test_support::{
    ApprovalOutcome, TestResult, approve_config, create_test_dir, load_hook_exports,
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

    let export = load_hook_exports(path, cuenv_bin)?;
    let exports = String::from_utf8(export.stdout)?;
    let check_script = format!(
        "{exports}\nif [ \"$SINGLE\" = \"success\" ]; then echo FOUND_SINGLE; else echo MISSING_SINGLE; fi; if [ \"$MULTI\" = \"line1\nline2\" ]; then echo FOUND_MULTI; else echo MISSING_MULTI; fi"
    );
    let output = Command::new("sh")
        .args(["-c", &check_script])
        .output()?;

    assert!(
        output.status.success(),
        "evaluating hook exports failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("FOUND_SINGLE") && stdout.contains("FOUND_MULTI"),
        "expected single-line and multiline hook exports, got: {stdout}"
    );
    Ok(())
}
