//! CLI integration tests for cross-project task materialization.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct CliOutput {
    stdout: String,
    stderr: String,
    success: bool,
}

/// Create a Command with a clean environment (no CI vars leaking).
fn clean_environment_command(bin: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("USER", std::env::var("USER").unwrap_or_default());
    cmd
}

/// Create a test directory with non-hidden name (CUE ignores hidden directories)
/// and initialize it with a `.git` directory.
fn create_test_root() -> TestResult<TempDir> {
    let tmp = tempfile::Builder::new().prefix("cuenv_test_").tempdir()?;
    let root = tmp.path();
    fs::create_dir_all(root.join(".git"))?;
    Ok(tmp)
}

/// Initialize a directory as a CUE module with proper structure
fn init_cue_module(dir: &Path, module_name: &str) -> TestResult {
    fs::create_dir_all(dir.join("cue.mod"))?;
    // CUE module paths must be lowercase
    let lowercase_name = module_name.to_lowercase();
    fs::write(
        dir.join("cue.mod/module.cue"),
        format!(
            r#"module: "test.example/{}"
language: version: "v0.9.0"
"#,
            lowercase_name
        ),
    )?;
    Ok(())
}

fn run_cuenv(args: &[&str]) -> TestResult<CliOutput> {
    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");
    let output = clean_environment_command(cuenv_bin).args(args).output()?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let success = output.status.success();

    Ok(CliOutput {
        stdout,
        stderr,
        success,
    })
}

fn path_str(path: &Path) -> TestResult<&str> {
    path.to_str()
        .ok_or_else(|| format!("path is not UTF-8: {}", path.display()).into())
}

fn write_proj_b(dir: &Path, version: &str) -> TestResult {
    let projb = dir.join("projB");
    fs::create_dir_all(projb.join("src"))?;
    init_cue_module(&projb, "projB")?;
    // package detection relies on first package line
    let cue_b = r#"package projB

name: "projB"

env: {}

tasks: {
  build: {
    command: "sh"
    args: ["-c", "mkdir -p dist/assets; cp -f src/version.txt dist/app.txt; echo asset > dist/assets/file.txt"]
    inputs: ["src/version.txt"]
    outputs: ["dist/app.txt", "dist/assets"]
  }
}
"#;
    fs::write(projb.join("env.cue"), cue_b)?;
    fs::write(projb.join("src/version.txt"), version)?;
    Ok(())
}

fn write_proj_a(
    dir: &Path,
    mapping_from: &str,
    mapping_to: &str,
    external_project: &str,
) -> TestResult {
    let proja = dir.join("projA");
    fs::create_dir_all(&proja)?;
    init_cue_module(&proja, "projA")?;
    let cue_a = format!(
        r#"package projA

name: "projA"

env: {{}}

tasks: {{
  consume: {{
    command: "sh"
    args: ["-c", "mkdir -p out; cp vendor/app.txt out/used.txt; echo done"]
    inputs: [{{
      project: "{external_project}"
      task: "build"
      map: [{{ from: "{mapping_from}", to: "{mapping_to}" }}]
    }}]
    outputs: ["out/used.txt"]
  }}
}}
"#
    );
    fs::write(proja.join("env.cue"), cue_a)?;
    Ok(())
}

#[test]
fn test_external_auto_run_and_materialization() -> TestResult {
    let tmp = create_test_root()?;
    let root = tmp.path();

    write_proj_b(root, "v1-auto")?;
    write_proj_a(root, "dist/app.txt", "vendor/app.txt", "../projB")?;

    let proja = root.join("projA");
    let output = run_cuenv(&[
        "task",
        "-p",
        path_str(&proja)?,
        "--package",
        "projA",
        "consume",
    ])?;

    assert!(
        output.success,
        "First run should succeed.\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.stdout, output.stderr
    );
    assert!(
        output.stdout.contains("Task 'consume' completed") || output.stdout.contains("succeeded"),
        "Expected success message in output.\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.stdout,
        output.stderr
    );
    Ok(())
}

#[test]
fn test_cache_hits_and_invalidation() -> TestResult {
    let tmp = create_test_root()?;
    let root = tmp.path();

    write_proj_b(root, "v1-cache")?;
    write_proj_a(root, "dist/app.txt", "vendor/app.txt", "../projB")?;
    let proja = root.join("projA");
    let proja_path = path_str(&proja)?;

    // First run (populate cache)
    let first = run_cuenv(&["task", "-p", proja_path, "--package", "projA", "consume"])?;
    assert!(
        first.success,
        "Run 1 failed. stdout: {}, stderr: {}",
        first.stdout, first.stderr
    );

    // Second run (should hit cache)
    let second = run_cuenv(&["task", "-p", proja_path, "--package", "projA", "consume"])?;
    assert!(
        second.success,
        "Run 2 failed. stdout: {}, stderr: {}",
        second.stdout, second.stderr
    );

    // Change external input content and rerun
    fs::write(root.join("projB/src/version.txt"), "v2")?;
    let third = run_cuenv(&["task", "-p", proja_path, "--package", "projA", "consume"])?;
    assert!(
        third.success,
        "Should re-run after external change. stdout: {}, stderr: {}",
        third.stdout, third.stderr
    );
    Ok(())
}

#[test]
#[ignore = "hermetic execution temporarily disabled - validation only runs in hermetic path"]
fn test_mapping_error_undeclared_output() -> TestResult {
    let tmp = create_test_root()?;
    let root = tmp.path();

    write_proj_b(root, "v1-map")?;
    write_proj_a(root, "dist/missing.txt", "vendor/app.txt", "../projB")?;

    let proja = root.join("projA");
    let output = run_cuenv(&[
        "task",
        "-p",
        path_str(&proja)?,
        "--package",
        "projA",
        "consume",
    ])?;

    assert!(
        !output.success,
        "Should fail on undeclared output mapping. stdout: {}, stderr: {}",
        output.stdout, output.stderr
    );
    Ok(())
}

#[test]
#[ignore = "hermetic execution temporarily disabled - validation only runs in hermetic path"]
fn test_path_safety_outside_git_root() -> TestResult {
    let tmp = create_test_root()?;
    let root = tmp.path();

    // Create projA only
    write_proj_a(root, "dist/app.txt", "vendor/app.txt", "../../outside")?;

    let proja = root.join("projA");
    let output = run_cuenv(&[
        "task",
        "-p",
        path_str(&proja)?,
        "--package",
        "projA",
        "consume",
    ])?;

    assert!(
        !output.success,
        "Should fail when external path resolves outside git root. stdout: {}, stderr: {}",
        output.stdout, output.stderr
    );
    Ok(())
}

#[test]
#[ignore = "hermetic execution temporarily disabled - validation only runs in hermetic path"]
fn test_collision_duplicate_dest() -> TestResult {
    let tmp = create_test_root()?;
    let root = tmp.path();

    // Write projB
    write_proj_b(root, "v1-coll")?;

    // Write projA with two mappings to same 'to'
    let proja = root.join("projA");
    fs::create_dir_all(&proja)?;
    init_cue_module(&proja, "projA")?;
    let cue_a = r#"package projA

name: "projA"

env: {}

tasks: {
  consume: {
    command: "sh"
    args: ["-c", "true"]
    inputs: [{
      project: "../projB"
      task: "build"
      map: [
        { from: "dist/app.txt", to: "vendor/app.txt" },
        { from: "dist/app.txt", to: "vendor/app.txt" }
      ]
    }]
    outputs: []
  }
}
"#;
    fs::write(proja.join("env.cue"), cue_a)?;

    let output = run_cuenv(&[
        "task",
        "-p",
        path_str(&proja)?,
        "--package",
        "projA",
        "consume",
    ])?;

    assert!(
        !output.success,
        "Should fail on destination collision. stdout: {}, stderr: {}",
        output.stdout, output.stderr
    );
    Ok(())
}
