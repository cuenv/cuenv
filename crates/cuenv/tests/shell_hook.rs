//! Integration test for onEnter hooks that include shellHook-style logic

mod hook_test_support;

use assert_cmd::Command;
use hook_test_support::{
    ApprovalOutcome, TestResult, approve_config, assert_sandbox_error, create_test_dir,
};
use std::fs;

#[test]
fn test_on_enter_shell_hook_exports() -> TestResult {
    let temp_dir = create_test_dir("test.example/shell-hook")?;
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
    fs::write(path.join("env.cue"), cue_content)?;

    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");

    match approve_config(path, cuenv_bin)? {
        ApprovalOutcome::Approved => {}
        ApprovalOutcome::SandboxError => return Ok(()),
    }

    let output = Command::new(cuenv_bin)
        .current_dir(path)
        .env("CUENV_EXECUTABLE", cuenv_bin)
        .args([
            "exec",
            "--",
            "sh",
            "-c",
            "if [ \"$SHELL_HOOK_VAR\" = \"from_shell_hook\" ]; then echo FOUND; else echo MISSING; exit 1; fi",
        ])
        .output()
        ?;

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
            stdout.contains("FOUND"),
            "Expected FOUND in stdout, got: {stdout}"
        );
    }
    Ok(())
}
