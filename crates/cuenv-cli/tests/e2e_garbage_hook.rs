use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_hook_with_syntax_error_output() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path();

    // Create env.cue with a hook that outputs a SYNTAX ERROR (unclosed quote)
    // This should cause the shell to abort and 'env -0' will probably not run or exit code will be non-zero.
    let cue_content = r#"
package cuenv

hooks: {
    onEnter: {
        command: "sh"
        args: ["-c", "echo 'export BAD=\"unclosed'; echo 'export GOOD=success'"]
        source: true
    }
}
"#;
    fs::write(path.join("env.cue"), cue_content).unwrap();
    
    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");

    let mut cmd = Command::cargo_bin("cuenv").unwrap();
    cmd.current_dir(path)
        .env("CUENV_EXECUTABLE", cuenv_bin)
        .arg("allow")
        .assert()
        .success();

    let mut cmd = Command::cargo_bin("cuenv").unwrap();
    let assert = cmd.current_dir(path)
        .env("CUENV_EXECUTABLE", cuenv_bin)
        .arg("exec")
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg("if [ \"$GOOD\" = \"success\" ]; then echo FOUND; else echo MISSING; exit 1; fi")
        .assert();

    // We expect this to FAIL to find the variable, because evaluation aborted.
    // But we want to know if it fails *silently* or logs the error.
    // In CLI test we can't easily check logs unless we capture stderr.
    // But we can verify that the variable is indeed missing.
    assert.failure() // Expect "MISSING" -> exit 1
        .stdout(predicates::str::contains("MISSING"));
}
