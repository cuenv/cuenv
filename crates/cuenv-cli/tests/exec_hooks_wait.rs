use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_exec_waits_for_hooks() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path();

    // Create env.cue with a slow hook that exports a variable
    let cue_content = r#"
package cuenv

hooks: {
    onEnter: {
        command: "sh"
        args: ["-c", "sleep 2 && echo export HOOK_VAR=success"]
        source: true
    }
}
"#;
    fs::write(path.join("env.cue"), cue_content).unwrap();

    // Get the path to the cuenv binary
    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");

    // 1. Approve the config
    let mut cmd = Command::cargo_bin("cuenv").unwrap();
    cmd.current_dir(path)
        .env("CUENV_EXECUTABLE", cuenv_bin) // Ensure supervisor uses correct binary
        .arg("allow")
        .assert()
        .success();

    // 2. Exec command that checks for the variable
    // We check that HOOK_VAR is "success".
    // Since the hook sleeps for 2s, and cuenv exec (currently) only waits 10ms,
    // this should fail if the bug exists.
    let mut cmd = Command::cargo_bin("cuenv").unwrap();
    let assert = cmd.current_dir(path)
        .env("CUENV_EXECUTABLE", cuenv_bin) // Ensure supervisor uses correct binary
        .arg("exec")
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg("if [ \"$HOOK_VAR\" = \"success\" ]; then echo FOUND; exit 0; else echo MISSING; exit 1; fi")
        .assert();

    // Assert success (fix verified)
    assert.success().stdout(predicates::str::contains("FOUND"));
}
