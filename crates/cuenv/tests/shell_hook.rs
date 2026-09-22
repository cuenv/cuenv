//! Integration test for onEnter hooks that include shellHook-style logic

mod hook_test_support;

use assert_cmd::Command;
use hook_test_support::{
    ApprovalOutcome, TestResult, approve_config, create_test_dir, load_hook_exports,
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

    let export = load_hook_exports(path, cuenv_bin)?;
    let exports = String::from_utf8(export.stdout)?;
    let check_script = format!(
        "{exports}\nif [ \"$BASE\" = \"ok\" ] && [ \"$SHELL_HOOK_VAR\" = \"from_shell_hook\" ]; then echo FOUND; else echo MISSING; exit 1; fi"
    );
    let output = Command::new("sh").args(["-c", &check_script]).output()?;

    assert!(
        output.status.success(),
        "evaluating hook exports failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("FOUND"),
        "expected source hook exports in stdout, got: {stdout}"
    );
    Ok(())
}
