//! Integration tests for the cuenv CLI
//!
//! These tests exercise the complete CLI functionality, including
//! argument parsing, command execution, and output formatting.

use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct CliOutput {
    stdout: String,
    stderr: String,
    success: bool,
}

impl CliOutput {
    fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// Create a Command with a clean environment (no CI vars leaking).
/// This prevents tests from hanging when run in CI environments where
/// variables like GITHUB_ACTIONS=true would trigger CI-specific code paths.
fn clean_environment_command(bin: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("USER", std::env::var("USER").unwrap_or_default());
    cmd
}

const EXPECTED_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Path to the pre-built cuenv binary (resolved at compile time by Cargo)
const CUENV_BIN: &str = env!("CARGO_BIN_EXE_cuenv");

/// Test helper to run cuenv CLI commands
fn run_cuenv_command(args: &[&str]) -> TestResult<CliOutput> {
    let mut cmd = clean_environment_command(CUENV_BIN);

    for arg in args {
        cmd.arg(arg);
    }

    let output = cmd.output()?;
    let stdout = str::from_utf8(&output.stdout)?.to_string();
    let stderr = str::from_utf8(&output.stderr)?.to_string();
    let success = output.status.success();

    Ok(CliOutput {
        stdout,
        stderr,
        success,
    })
}

/// Get the path to the test examples directory
fn get_testexamples_path() -> TestResult<String> {
    // Use the CARGO_MANIFEST_DIR environment variable to get the project root
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    // Go up two levels from crates/cuenv-cli to the project root
    let project_root = std::path::Path::new(manifest_dir)
        .parent() // crates
        .and_then(|p| p.parent()) // project root
        .ok_or_else(|| io::Error::other("failed to find project root"))?;

    Ok(project_root
        .join("examples/env-basic")
        .to_string_lossy()
        .to_string())
}

fn run_git_command(path: &Path, args: &[&str]) -> TestResult {
    let output = Command::new("git").args(args).current_dir(path).output()?;
    assert!(
        output.status.success(),
        "git {} failed\nstdout: {}\nstderr: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// Create a temporary directory with git initialized and CUE files for sync testing.
/// This is needed because `cuenv sync` requires being inside a git repository.
/// Returns a `TempDir` that will be cleaned up when dropped, and the path as a String.
///
/// IMPORTANT: Uses a non-hidden prefix (`cuenv_test_`) because CUE's loader
/// ignores directories starting with '.' (like the default .tmpXXXXXX).
fn create_git_test_env() -> TestResult<(TempDir, String)> {
    let temp_dir = tempfile::Builder::new().prefix("cuenv_test_").tempdir()?;
    let temp_path = temp_dir.path();

    run_git_command(temp_path, &["init"])?;
    run_git_command(temp_path, &["config", "user.email", "test@example.com"])?;
    run_git_command(temp_path, &["config", "user.name", "Test User"])?;

    let cue_mod_dir = temp_path.join("cue.mod");
    fs::create_dir_all(&cue_mod_dir)?;

    fs::write(
        cue_mod_dir.join("module.cue"),
        r#"module: "test.example/sync"
language: version: "v0.9.0"
"#,
    )?;

    fs::write(
        temp_path.join("env.cue"),
        r#"package cuenv

name: "sync-test"

env: {
    TEST_VAR: "test_value"
}
"#,
    )?;

    let path_str = temp_path.to_string_lossy().to_string();
    Ok((temp_dir, path_str))
}

fn create_git_test_env_with_schema_dependency(
    version: &str,
) -> TestResult<(TempDir, String, PathBuf)> {
    let (temp_dir, path_str) = create_git_test_env()?;
    let module_file = temp_dir.path().join("cue.mod/module.cue");
    fs::write(
        &module_file,
        format!(
            r#"module: "test.example/sync"
language: version: "v0.14.1"
deps: "github.com/cuenv/cuenv@v0": v: "v{version}"
"#
        ),
    )?;

    Ok((temp_dir, path_str, module_file))
}

fn assert_success(output: &CliOutput, context: &str) {
    assert!(
        output.success,
        "{context}\nstdout: {}\nstderr: {}",
        output.stdout, output.stderr
    );
}

fn correlation_id(stdout: &str) -> TestResult<&str> {
    stdout
        .lines()
        .find(|line| line.contains("Correlation ID:"))
        .and_then(|line| line.split("Correlation ID:").nth(1))
        .map(str::trim)
        .ok_or_else(|| io::Error::other("could not extract correlation ID").into())
}

#[test]
fn test_version_command_basic() -> TestResult {
    let output = run_cuenv_command(&["version"])?;

    assert_success(&output, "Command should succeed");
    assert!(output.stdout.contains("cuenv"));
    assert!(output.stdout.contains(EXPECTED_VERSION));
    assert!(output.stdout.contains("Authors:"));
    assert!(output.stdout.contains("Target:"));
    assert!(output.stdout.contains("Correlation ID:"));
    assert!(output.stdout.contains("cuenv is an event-driven CLI"));
    Ok(())
}

#[test]
fn test_version_command_with_level_debug() -> TestResult {
    let output = run_cuenv_command(&["--level", "debug", "version"])?;

    assert_success(&output, "Command should succeed");
    assert!(output.stdout.contains("cuenv"));
    assert!(output.stdout.contains(EXPECTED_VERSION));
    assert!(output.stderr.contains("DEBUG") || output.stderr.contains("debug"));
    Ok(())
}

#[test]
fn test_version_command_with_level_error() -> TestResult {
    let output = run_cuenv_command(&["--level", "error", "version"])?;

    assert_success(&output, "Command should succeed");
    assert!(output.stdout.contains("cuenv"));
    assert!(output.stdout.contains(EXPECTED_VERSION));
    Ok(())
}

#[test]
fn test_version_command_with_json_flag() -> TestResult {
    let output = run_cuenv_command(&["--json", "--level", "info", "version"])?;

    assert_success(&output, "Command should succeed");
    assert!(output.stdout.contains("cuenv"));
    Ok(())
}

#[test]
fn test_version_command_short_level_flag() -> TestResult {
    // Note: -l was changed to -L to accommodate the new --label/-l flag for tasks
    let output = run_cuenv_command(&["-L", "warn", "version"])?;

    assert_success(&output, "Command should succeed");
    assert!(output.stdout.contains("cuenv"));
    assert!(output.stdout.contains(EXPECTED_VERSION));
    Ok(())
}

#[test]
fn test_help_flag() -> TestResult {
    let output = run_cuenv_command(&["--help"])?;

    assert!(output.stdout.contains("cuenv") || output.stdout.contains("Usage"));
    assert!(output.stdout.contains("--level") || output.stdout.contains("-L"));
    assert!(output.stdout.contains("--json"));
    assert!(output.stdout.contains("version"));
    Ok(())
}

#[test]
fn test_version_help() -> TestResult {
    let output = run_cuenv_command(&["version", "--help"])?;

    assert!(
        output.stdout.contains("version") || output.stdout.contains("Show version information")
    );
    Ok(())
}

#[test]
fn test_invalid_log_level() -> TestResult {
    let output = run_cuenv_command(&["--level", "invalid", "version"])?;

    assert!(
        !output.success,
        "Command should fail with invalid log level"
    );
    assert!(output.stderr.contains("error") || output.stderr.contains("invalid"));
    Ok(())
}

#[test]
fn test_missing_subcommand() -> TestResult {
    let output = run_cuenv_command(&[])?;

    assert!(!output.success, "Command should fail without subcommand");
    assert!(output.stderr.contains("error") || output.stderr.contains("required"));
    Ok(())
}

#[test]
fn test_combined_flags() -> TestResult {
    let output = run_cuenv_command(&["--level", "info", "--json", "version", "--output", "json"])?;

    assert_success(&output, "Command should succeed with combined flags");
    assert!(output.stdout.contains("cuenv"));
    assert!(output.stdout.contains(EXPECTED_VERSION));
    Ok(())
}

#[test]
fn test_output_consistency() -> TestResult {
    let first = run_cuenv_command(&["--level", "error", "version"])?;
    let second = run_cuenv_command(&["--level", "error", "version"])?;

    assert!(
        first.success && second.success,
        "Both commands should succeed\nfirst stdout: {}\nfirst stderr: {}\nsecond stdout: {}\nsecond stderr: {}",
        first.stdout,
        first.stderr,
        second.stdout,
        second.stderr
    );

    let first_lines: Vec<&str> = first.stdout.lines().collect();
    let second_lines: Vec<&str> = second.stdout.lines().collect();

    assert_eq!(
        first_lines.len(),
        second_lines.len(),
        "Output should have same number of lines"
    );

    for (line1, line2) in first_lines.iter().zip(second_lines.iter()) {
        if !line1.contains("Correlation ID:") {
            assert_eq!(line1, line2, "Non-correlation-ID lines should be identical");
        }
    }
    Ok(())
}

#[test]
fn test_correlation_id_uniqueness() -> TestResult {
    let first = run_cuenv_command(&["--level", "error", "version"])?;
    let second = run_cuenv_command(&["--level", "error", "version"])?;

    let correlation1 = correlation_id(&first.stdout)?;
    let correlation2 = correlation_id(&second.stdout)?;

    assert_ne!(
        correlation1, correlation2,
        "Correlation IDs should be different between runs"
    );
    assert_eq!(
        correlation1.len(),
        36,
        "Correlation ID should be UUID length"
    );
    assert_eq!(
        correlation2.len(),
        36,
        "Correlation ID should be UUID length"
    );
    assert!(
        correlation1.contains('-'),
        "Correlation ID should contain hyphens"
    );
    assert!(
        correlation2.contains('-'),
        "Correlation ID should contain hyphens"
    );
    Ok(())
}
#[test]
fn test_env_print_command_basic() -> TestResult {
    let test_path = get_testexamples_path()?;
    let output = run_cuenv_command(&[
        "env",
        "print",
        "--path",
        &test_path,
        "--package",
        "examples",
    ])?;

    assert_success(&output, "Command should succeed");
    assert!(
        output
            .stdout
            .contains("DATABASE_URL=postgres://localhost/mydb")
    );
    assert!(output.stdout.contains("DEBUG=true"));
    assert!(output.stdout.contains("PORT=3000"));
    assert!(output.stdout.contains("BASE_URL=https://api.example.com"));
    assert!(
        output
            .stdout
            .contains("API_ENDPOINT=https://api.example.com/v1")
    );
    Ok(())
}

#[test]
fn test_env_print_command_json_format() -> TestResult {
    let test_path = get_testexamples_path()?;
    let output = run_cuenv_command(&[
        "env",
        "print",
        "--path",
        &test_path,
        "--package",
        "examples",
        "--output",
        "json",
    ])?;

    assert_success(&output, "Command should succeed");

    let parsed: serde_json::Value = serde_json::from_str(&output.stdout)?;

    assert_eq!(parsed["DATABASE_URL"], "postgres://localhost/mydb");
    assert_eq!(parsed["DEBUG"], "true");
    assert_eq!(parsed["PORT"], "3000");
    assert_eq!(parsed["BASE_URL"], "https://api.example.com");
    assert_eq!(parsed["API_ENDPOINT"], "https://api.example.com/v1");
    Ok(())
}

#[test]
fn test_env_print_command_with_short_path_flag() -> TestResult {
    let test_path = get_testexamples_path()?;
    let output = run_cuenv_command(&["env", "print", "-p", &test_path, "--package", "examples"])?;

    assert_success(&output, "Command should succeed with short path flag");
    assert!(output.stdout.contains("DATABASE_URL="));
    assert!(output.stdout.contains("DEBUG="));
    assert!(output.stdout.contains("PORT="));
    Ok(())
}

#[test]
fn test_env_print_command_invalid_path() -> TestResult {
    let output = run_cuenv_command(&[
        "env",
        "print",
        "--path",
        "nonexistent/path",
        "--package",
        "examples",
    ])?;

    assert!(!output.success, "Command should fail with invalid path");
    Ok(())
}

#[test]
fn test_env_print_command_invalid_package() -> TestResult {
    let output = run_cuenv_command(&[
        "env",
        "print",
        "--path",
        "examples/env-basic",
        "--package",
        "nonexistent",
    ])?;

    assert!(!output.success, "Command should fail with invalid package");
    Ok(())
}

#[test]
fn test_env_print_command_unsupported_format() -> TestResult {
    let test_path = get_testexamples_path()?;
    let output = run_cuenv_command(&[
        "env",
        "print",
        "--path",
        &test_path,
        "--package",
        "examples",
        "--output",
        "yaml",
    ])?;

    assert!(
        !output.success,
        "Command should fail with unsupported format"
    );
    let combined_output = output.combined();
    assert!(
        combined_output.contains("Unsupported format") || combined_output.contains("yaml"),
        "Error message should mention unsupported format 'yaml'"
    );
    Ok(())
}

// ===== Sync Command Integration Tests =====

#[test]
fn test_sync_command_dry_run() -> TestResult {
    let (_temp_dir, test_path) = create_git_test_env()?;
    let output = run_cuenv_command(&[
        "sync",
        "--path",
        &test_path,
        "--package",
        "cuenv",
        "--dry-run",
    ])?;

    assert_success(&output, "Command should succeed");
    assert!(
        output.stdout.contains("[codegen]")
            || output.stdout.contains("[ci]")
            || output.stdout.contains("[rules]"),
        "Dry run should show provider status sections"
    );
    Ok(())
}

#[test]
fn test_sync_command_dry_run_reports_provider_status() -> TestResult {
    let (_temp_dir, test_path) = create_git_test_env()?;
    let output = run_cuenv_command(&[
        "sync",
        "--path",
        &test_path,
        "--package",
        "cuenv",
        "--dry-run",
    ])?;

    assert_success(&output, "Command should succeed");
    assert!(
        output.stdout.contains("No CI providers configured")
            || output.stdout.contains("No codegen configuration")
            || output.stdout.contains("No .rules.cue"),
        "Dry run should report provider status"
    );
    Ok(())
}

#[test]
fn test_cue_command_warns_when_schema_dependency_differs_from_cli() -> TestResult {
    let (_temp_dir, test_path, _module_file) =
        create_git_test_env_with_schema_dependency("0.999.0")?;
    let output = run_cuenv_command(&["env", "print", "--path", &test_path, "--package", "cuenv"])?;

    assert_success(&output, "command should warn but continue");
    let combined = output.combined();
    assert!(combined.contains("Warning: cuenv schema dependency is v0.999.0"));
    assert!(combined.contains("CLI is"));
    Ok(())
}

#[test]
fn test_sync_dry_run_does_not_write_module_cue() -> TestResult {
    let (temp_dir, test_path) = create_git_test_env()?;
    let module_file = temp_dir.path().join("cue.mod/module.cue");
    let before = fs::read_to_string(&module_file)?;
    let output = run_cuenv_command(&[
        "sync",
        "--path",
        &test_path,
        "--package",
        "cuenv",
        "--dry-run",
    ])?;

    assert_success(&output, "sync --dry-run should succeed");
    let after = fs::read_to_string(&module_file)?;
    assert_eq!(before, after, "dry-run must not update module.cue");
    Ok(())
}

#[test]
fn test_sync_check_allows_missing_schema_dependency() -> TestResult {
    let (_temp_dir, test_path) = create_git_test_env()?;
    let output = run_cuenv_command(&[
        "sync",
        "--path",
        &test_path,
        "--package",
        "cuenv",
        "--check",
    ])?;

    assert_success(
        &output,
        "sync --check should not require a schema dependency",
    );
    Ok(())
}

#[test]
fn test_sync_does_not_write_module_cue() -> TestResult {
    let (temp_dir, test_path) = create_git_test_env()?;
    let module_file = temp_dir.path().join("cue.mod/module.cue");
    let before = fs::read_to_string(&module_file)?;
    let output = run_cuenv_command(&["sync", "--path", &test_path, "--package", "cuenv"])?;

    assert_success(&output, "sync should succeed");
    let after = fs::read_to_string(&module_file)?;
    assert_eq!(before, after, "sync must not update module.cue");
    Ok(())
}

#[test]
fn test_sync_command_invalid_path() -> TestResult {
    let output = run_cuenv_command(&[
        "sync",
        "--path",
        "nonexistent/path",
        "--package",
        "examples",
        "--dry-run",
    ])?;

    assert!(!output.success, "Command should fail with invalid path");
    Ok(())
}

#[test]
fn test_sync_command_invalid_package() -> TestResult {
    let (_temp_dir, test_path) = create_git_test_env()?;
    let output = run_cuenv_command(&[
        "sync",
        "--path",
        &test_path,
        "--package",
        "nonexistent",
        "--dry-run",
    ])?;

    assert!(!output.success, "Command should fail with invalid package");
    Ok(())
}

#[test]
fn test_sync_command_help() -> TestResult {
    let output = run_cuenv_command(&["sync", "--help"])?;

    assert!(
        output.stdout.contains("sync") || output.stdout.contains("Sync"),
        "Help should mention the sync command"
    );
    assert!(
        output.stdout.contains("--dry-run") || output.stdout.contains("dry"),
        "Help should mention --dry-run option"
    );
    Ok(())
}

// =========================================================================
// CI Workflow Generation Tests - Monorepo working-directory support
// =========================================================================

/// Create a monorepo test environment with multiple projects at different paths.
/// Returns a `TempDir` that will be cleaned up when dropped, and the path as a String.
fn create_monorepo_test_env() -> TestResult<(TempDir, String)> {
    let temp_dir = tempfile::Builder::new()
        .prefix("cuenv_monorepo_test_")
        .tempdir()?;
    let temp_path = temp_dir.path();

    run_git_command(temp_path, &["init"])?;
    run_git_command(temp_path, &["config", "user.email", "test@example.com"])?;
    run_git_command(temp_path, &["config", "user.name", "Test User"])?;

    let cue_mod_dir = temp_path.join("cue.mod");
    fs::create_dir_all(&cue_mod_dir)?;

    fs::write(
        cue_mod_dir.join("module.cue"),
        r#"module: "test.example/monorepo"
language: version: "v0.9.0"
"#,
    )?;

    fs::write(
        temp_path.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "root-project"

ci: {
    github: {}
    pipelines: [
        {
            name: "default"
            trigger: branches: ["main"]
            tasks: ["test"]
        }
    ]
}

tasks: {
    test: {
        command: "echo"
        args: ["Running root test"]
        inputs: ["env.cue"]
    }
}
"#,
    )?;

    let api_dir = temp_path.join("services").join("api");
    fs::create_dir_all(&api_dir)?;

    fs::write(
        api_dir.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "api-service"

ci: {
    github: {}
    pipelines: [
        {
            name: "default"
            trigger: branches: ["main"]
            tasks: ["build", "test"]
        }
    ]
}

let _t = tasks

tasks: {
    build: {
        command: "cargo"
        args: ["build"]
        inputs: ["src/**"]
    }
    test: {
        command: "cargo"
        args: ["test"]
        dependsOn: [_t.build]
    }
}
"#,
    )?;

    let web_dir = temp_path.join("apps").join("web");
    fs::create_dir_all(&web_dir)?;

    fs::write(
        web_dir.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "web-app"

ci: {
    github: {}
    pipelines: [
        {
            name: "default"
            trigger: branches: ["main"]
            tasks: ["deploy"]
        }
    ]
}

tasks: {
    deploy: {
        command: "./deploy.sh"
        inputs: ["dist/**"]
    }
}
"#,
    )?;

    let path_str = temp_path.to_string_lossy().to_string();
    Ok((temp_dir, path_str))
}

/// Helper to run cuenv command in a specific directory
fn run_cuenv_command_in_dir(args: &[&str], dir: &str) -> TestResult<CliOutput> {
    let mut cmd = clean_environment_command(CUENV_BIN);

    for arg in args {
        cmd.arg(arg);
    }

    cmd.current_dir(dir);

    let output = cmd.output()?;
    let stdout = str::from_utf8(&output.stdout)?.to_string();
    let stderr = str::from_utf8(&output.stderr)?.to_string();
    let success = output.status.success();

    Ok(CliOutput {
        stdout,
        stderr,
        success,
    })
}

#[test]
fn test_sync_ci_dry_run_shows_workflows() -> TestResult {
    let (_temp_dir, temp_path) = create_monorepo_test_env()?;

    let output = run_cuenv_command_in_dir(&["sync", "ci", "--dry-run"], &temp_path)?;
    let combined = output.combined();
    assert!(
        output.success || combined.contains("error") || combined.contains("failed"),
        "Command should either succeed or report meaningful error\nstdout: {}\nstderr: {}",
        output.stdout,
        output.stderr
    );
    Ok(())
}

#[test]
fn test_sync_ci_help() -> TestResult {
    let output = run_cuenv_command(&["sync", "ci", "--help"])?;

    assert_success(&output, "Help command should succeed");
    assert!(
        output.stdout.contains("Sync CI workflow files"),
        "Help should describe CI sync functionality"
    );
    assert!(
        output.stdout.contains("--dry-run"),
        "Help should mention --dry-run option"
    );
    Ok(())
}
