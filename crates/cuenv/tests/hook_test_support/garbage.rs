//! Integration test for hooks with syntax errors

use super::{ApprovalOutcome, TestResult, approve_config, create_test_dir, run_cuenv};
use std::fs;

#[test]
fn test_hook_with_syntax_error_output() -> TestResult {
    let temp_dir = create_test_dir("test.example/garbage")?;
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
    fs::write(path.join("env.cue"), cue_content)?;

    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");

    match approve_config(path, cuenv_bin)? {
        ApprovalOutcome::Approved => {}
        ApprovalOutcome::SandboxError => return Ok(()),
    }

    let load = run_cuenv(path, cuenv_bin, &["env", "load"])?;
    assert!(
        load.status.success(),
        "cuenv env load failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&load.stdout),
        String::from_utf8_lossy(&load.stderr)
    );

    let status = run_cuenv(
        path,
        cuenv_bin,
        &[
            "env",
            "status",
            "--wait",
            "--timeout",
            "10",
            "--output",
            "short",
        ],
    )?;
    assert!(
        status.status.success(),
        "cuenv env status failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&status.stdout).trim(),
        "[ERR]",
        "syntax-error hook should fail"
    );

    let export = run_cuenv(path, cuenv_bin, &["export", "--shell", "bash"])?;
    assert!(
        export.status.success(),
        "cuenv export failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&export.stdout),
        String::from_utf8_lossy(&export.stderr)
    );
    let stdout = String::from_utf8_lossy(&export.stdout);
    assert!(
        !stdout.contains("BAD=") && !stdout.contains("GOOD="),
        "failed hooks must not export partial environment: {stdout}"
    );
    Ok(())
}
