//! Integration test for exec shell-hook isolation.

use assert_cmd::Command;
use std::error::Error;
use std::fs;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn create_test_dir(module: &str) -> TestResult<TempDir> {
    let temp_dir = tempfile::Builder::new().prefix("cuenv_test_").tempdir()?;
    let path = temp_dir.path();
    fs::create_dir_all(path.join("cue.mod"))?;
    fs::write(
        path.join("cue.mod/module.cue"),
        format!("module: \"{module}\"\nlanguage: version: \"v0.9.0\"\n"),
    )?;
    Ok(temp_dir)
}

#[test]
fn test_exec_does_not_run_or_warn_about_unapproved_shell_hooks() -> TestResult {
    let temp_dir = create_test_dir("test.example/hooks")?;
    let path = temp_dir.path();

    fs::write(
        path.join("env.cue"),
        r#"package cuenv

name: "test"

env: {
    STATIC_VAR: "static-value"
}

hooks: {
    onEnter: {
        shell_only: {
            command: "sh"
            args: ["-c", "touch hook-ran && echo export HOOK_VAR=from-hook"]
            source: true
        }
    }
}
"#,
    )?;

    let hook_var_expr = "$".to_owned() + "{HOOK_VAR:-missing}";
    let check_script = format!("printf '%s:%s\n' \"$STATIC_VAR\" \"{hook_var_expr}\"");

    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");
    let output = Command::new(cuenv_bin)
        .current_dir(path)
        .env("CUENV_EXECUTABLE", cuenv_bin)
        .args(["exec", "--", "sh", "-c", &check_script])
        .output()?;

    assert!(
        output.status.success(),
        "exec should succeed without hook approval\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("static-value:missing"),
        "exec should receive static env but not hook-generated env\nstdout:\n{stdout}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Hooks not run") && !stderr.contains("approval required"),
        "exec should not print shell hook approval guidance\nstderr:\n{stderr}"
    );
    assert!(
        !path.join("hook-ran").exists(),
        "exec should not execute onEnter shell hooks"
    );

    Ok(())
}
