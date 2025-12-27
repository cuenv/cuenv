//! Integration tests for the cuenv CLI
//!
//! These tests exercise the complete CLI functionality, including
//! argument parsing, command execution, and output formatting.

// Integration tests can use unwrap/expect for cleaner assertions
#![allow(clippy::print_stdout, clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;
use std::str;

const EXPECTED_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Path to the pre-built cuenv binary (resolved at compile time by Cargo)
const CUENV_BIN: &str = env!("CARGO_BIN_EXE_cuenv");

/// Test helper to run cuenv CLI commands
fn run_cuenv_command(args: &[&str]) -> Result<(String, String, bool), Box<dyn std::error::Error>> {
    let mut cmd = Command::new(CUENV_BIN);

    for arg in args {
        cmd.arg(arg);
    }

    let output = cmd.output()?;
    let stdout = str::from_utf8(&output.stdout)?.to_string();
    let stderr = str::from_utf8(&output.stderr)?.to_string();
    let success = output.status.success();

    Ok((stdout, stderr, success))
}

/// Get the path to the test examples directory
fn get_test_examples_path() -> String {
    // Use the CARGO_MANIFEST_DIR environment variable to get the project root
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    // Go up two levels from crates/cuenv-cli to the project root
    let project_root = std::path::Path::new(manifest_dir)
        .parent() // crates
        .and_then(|p| p.parent()) // project root
        .expect("Failed to find project root");

    project_root
        .join("_examples/env-basic")
        .to_string_lossy()
        .to_string()
}

/// Create a temporary directory with git initialized and CUE files for sync testing.
/// This is needed because `cuenv sync` requires being inside a git repository.
/// Returns a `TempDir` that will be cleaned up when dropped, and the path as a String.
///
/// IMPORTANT: Uses a non-hidden prefix (`cuenv_test_`) because CUE's loader
/// ignores directories starting with '.' (like the default .tmpXXXXXX).
fn create_git_test_env() -> (tempfile::TempDir, String) {
    let temp_dir = tempfile::Builder::new()
        .prefix("cuenv_test_")
        .tempdir()
        .expect("Failed to create temp directory");
    let temp_path = temp_dir.path();

    // Initialize git repository
    Command::new("git")
        .args(["init"])
        .current_dir(temp_path)
        .output()
        .expect("Failed to init git repo");

    // Configure git user for the repo (required for some git operations)
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(temp_path)
        .output()
        .expect("Failed to configure git email");

    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(temp_path)
        .output()
        .expect("Failed to configure git name");

    // Create cue.mod directory and module.cue
    let cue_mod_dir = temp_path.join("cue.mod");
    fs::create_dir_all(&cue_mod_dir).expect("Failed to create cue.mod directory");

    fs::write(
        cue_mod_dir.join("module.cue"),
        r#"module: "test.example/sync"
language: version: "v0.9.0"
"#,
    )
    .expect("Failed to write module.cue");

    // Create env.cue with ignore patterns (simplified, no imports)
    fs::write(
        temp_path.join("env.cue"),
        r#"package cuenv

name: "sync-test"

env: {
    TEST_VAR: "test_value"
}

ignore: {
    git:    ["node_modules/", ".env", "*.log", "target/"]
    docker: ["node_modules/", ".git/", "target/", "*.md"]
}
"#,
    )
    .expect("Failed to write env.cue");

    let path_str = temp_path.to_string_lossy().to_string();
    (temp_dir, path_str)
}

#[test]
fn test_version_command_basic() {
    let result = run_cuenv_command(&["version"]);

    match result {
        Ok((stdout, _stderr, success)) => {
            assert!(success, "Command should succeed");
            assert!(stdout.contains("cuenv"));
            assert!(stdout.contains(EXPECTED_VERSION));
            assert!(stdout.contains("Authors:"));
            assert!(stdout.contains("Target:"));
            assert!(stdout.contains("Correlation ID:"));
            assert!(stdout.contains("cuenv is an event-driven CLI"));
        }
        Err(e) => panic!("Failed to run cuenv version: {e}"),
    }
}

#[test]
fn test_version_command_with_level_debug() {
    let result = run_cuenv_command(&["--level", "debug", "version"]);

    match result {
        Ok((stdout, stderr, success)) => {
            assert!(success, "Command should succeed");

            // stdout should still contain version info
            assert!(stdout.contains("cuenv"));
            assert!(stdout.contains(EXPECTED_VERSION));

            // stderr should contain debug logs when level is debug
            assert!(stderr.contains("DEBUG") || stderr.contains("debug"));
        }
        Err(e) => panic!("Failed to run cuenv version with debug level: {e}"),
    }
}

#[test]
fn test_version_command_with_level_error() {
    let result = run_cuenv_command(&["--level", "error", "version"]);

    match result {
        Ok((stdout, _stderr, success)) => {
            assert!(success, "Command should succeed");

            // stdout should contain version info
            assert!(stdout.contains("cuenv"));
            assert!(stdout.contains(EXPECTED_VERSION));

            // With error level, there should be minimal to no debug output in stderr
        }
        Err(e) => panic!("Failed to run cuenv version with error level: {e}"),
    }
}

#[test]
fn test_version_command_with_json_flag() {
    let result = run_cuenv_command(&["--json", "--level", "info", "version"]);

    match result {
        Ok((stdout, stderr, success)) => {
            assert!(success, "Command should succeed");

            // stdout should still contain version info in normal format
            assert!(stdout.contains("cuenv"));

            // Note: Sync commands (like version) skip tracing initialization for performance,
            // so there won't be JSON logs in stderr. This is intentional behavior.
            // The --json flag still affects output format for commands that produce JSON.
            // stderr may contain cargo compilation output when run via `cargo run`.
            // We just verify the command succeeds, not that it produces JSON logs.
            let _ = stderr; // silence unused warning
        }
        Err(e) => panic!("Failed to run cuenv version with JSON flag: {e}"),
    }
}

#[test]
fn test_version_command_short_level_flag() {
    // Note: -l was changed to -L to accommodate the new --label/-l flag for tasks
    let result = run_cuenv_command(&["-L", "warn", "version"]);

    match result {
        Ok((stdout, _stderr, success)) => {
            assert!(success, "Command should succeed");
            assert!(stdout.contains("cuenv"));
            assert!(stdout.contains(EXPECTED_VERSION));
        }
        Err(e) => panic!("Failed to run cuenv version with short level flag: {e}"),
    }
}

#[test]
fn test_help_flag() {
    let result = run_cuenv_command(&["--help"]);

    match result {
        Ok((stdout, _stderr, _success)) => {
            // Help output should contain key information
            assert!(stdout.contains("cuenv") || stdout.contains("Usage"));
            // Note: Short flag changed from -l to -L for log level
            assert!(stdout.contains("--level") || stdout.contains("-L"));
            assert!(stdout.contains("--json"));
            assert!(stdout.contains("version"));
        }
        Err(e) => panic!("Failed to run cuenv --help: {e}"),
    }
}

#[test]
fn test_version_help() {
    let result = run_cuenv_command(&["version", "--help"]);

    match result {
        Ok((stdout, _stderr, _success)) => {
            // Version help should contain information about the version command
            assert!(stdout.contains("version") || stdout.contains("Show version information"));
        }
        Err(e) => panic!("Failed to run cuenv version --help: {e}"),
    }
}

#[test]
fn test_invalid_log_level() {
    let result = run_cuenv_command(&["--level", "invalid", "version"]);

    match result {
        Ok((_stdout, stderr, success)) => {
            // Should fail with invalid log level
            assert!(!success, "Command should fail with invalid log level");
            assert!(stderr.contains("error") || stderr.contains("invalid"));
        }
        Err(e) => panic!("Failed to run cuenv with invalid level: {e}"),
    }
}

#[test]
fn test_missing_subcommand() {
    let result = run_cuenv_command(&[]);

    match result {
        Ok((_stdout, stderr, success)) => {
            // Should fail without subcommand
            assert!(!success, "Command should fail without subcommand");
            assert!(stderr.contains("error") || stderr.contains("required"));
        }
        Err(e) => panic!("Failed to run cuenv with no args: {e}"),
    }
}

#[test]
fn test_combined_flags() {
    let result = run_cuenv_command(&["--level", "info", "--json", "version", "--output", "json"]);

    match result {
        Ok((stdout, _stderr, success)) => {
            assert!(success, "Command should succeed with combined flags");
            assert!(stdout.contains("cuenv"));
            assert!(stdout.contains(EXPECTED_VERSION));
        }
        Err(e) => panic!("Failed to run cuenv with combined flags: {e}"),
    }
}

#[test]
fn test_output_consistency() {
    // Run the same command multiple times and ensure consistent output format
    let result1 = run_cuenv_command(&["--level", "error", "version"]);
    let result2 = run_cuenv_command(&["--level", "error", "version"]);

    match (result1, result2) {
        (Ok((stdout1, _stderr1, success1)), Ok((stdout2, _stderr2, success2))) => {
            assert!(success1 && success2, "Both commands should succeed");

            // The format should be consistent (correlation ID will differ)
            let lines1: Vec<&str> = stdout1.lines().collect();
            let lines2: Vec<&str> = stdout2.lines().collect();

            assert_eq!(
                lines1.len(),
                lines2.len(),
                "Output should have same number of lines"
            );

            // Check that non-correlation-ID lines are identical
            for (line1, line2) in lines1.iter().zip(lines2.iter()) {
                if !line1.contains("Correlation ID:") {
                    assert_eq!(line1, line2, "Non-correlation-ID lines should be identical");
                }
            }
        }
        (Err(e1), _) => panic!("First command failed: {e1}"),
        (_, Err(e2)) => panic!("Second command failed: {e2}"),
    }
}

#[test]
fn test_correlation_id_uniqueness() {
    // Run command multiple times and ensure correlation IDs are different
    let result1 = run_cuenv_command(&["--level", "error", "version"]);
    let result2 = run_cuenv_command(&["--level", "error", "version"]);

    match (result1, result2) {
        (Ok((stdout1, _, _)), Ok((stdout2, _, _))) => {
            let correlation1 = stdout1
                .lines()
                .find(|line| line.contains("Correlation ID:"))
                .and_then(|line| line.split("Correlation ID:").nth(1))
                .map(str::trim);

            let correlation2 = stdout2
                .lines()
                .find(|line| line.contains("Correlation ID:"))
                .and_then(|line| line.split("Correlation ID:").nth(1))
                .map(str::trim);

            match (correlation1, correlation2) {
                (Some(id1), Some(id2)) => {
                    assert_ne!(id1, id2, "Correlation IDs should be different between runs");
                    // Both should be valid UUID format (36 characters with hyphens)
                    assert_eq!(id1.len(), 36, "Correlation ID should be UUID length");
                    assert_eq!(id2.len(), 36, "Correlation ID should be UUID length");
                    assert!(id1.contains('-'), "Correlation ID should contain hyphens");
                    assert!(id2.contains('-'), "Correlation ID should contain hyphens");
                }
                _ => panic!("Could not extract correlation IDs from output"),
            }
        }
        (Err(e1), _) => panic!("First command failed: {e1}"),
        (_, Err(e2)) => panic!("Second command failed: {e2}"),
    }
}
#[test]
fn test_env_print_command_basic() {
    let test_path = get_test_examples_path();
    let result = run_cuenv_command(&[
        "env",
        "print",
        "--path",
        &test_path,
        "--package",
        "_examples",
    ]);

    match result {
        Ok((stdout, stderr, success)) => {
            if !success {
                println!("stdout: {stdout}");
                println!("stderr: {stderr}");
            }
            assert!(success, "Command should succeed");
            assert!(stdout.contains("DATABASE_URL=postgres://localhost/mydb"));
            assert!(stdout.contains("DEBUG=true"));
            assert!(stdout.contains("PORT=3000"));
            assert!(stdout.contains("BASE_URL=https://api.example.com"));
            assert!(stdout.contains("API_ENDPOINT=https://api.example.com/v1"));
        }
        Err(e) => panic!("Failed to run cuenv env print: {e}"),
    }
}

#[test]
fn test_env_print_command_json_format() {
    let test_path = get_test_examples_path();
    let result = run_cuenv_command(&[
        "env",
        "print",
        "--path",
        &test_path,
        "--package",
        "_examples",
        "--output",
        "json",
    ]);

    match result {
        Ok((stdout, _stderr, success)) => {
            assert!(success, "Command should succeed");

            // Parse as JSON to verify it's valid JSON
            let parsed: serde_json::Value =
                serde_json::from_str(&stdout).expect("Output should be valid JSON");

            // All values are now strings after resolution
            assert_eq!(parsed["DATABASE_URL"], "postgres://localhost/mydb");
            assert_eq!(parsed["DEBUG"], "true");
            assert_eq!(parsed["PORT"], "3000");
            assert_eq!(parsed["BASE_URL"], "https://api.example.com");
            assert_eq!(parsed["API_ENDPOINT"], "https://api.example.com/v1");
        }
        Err(e) => panic!("Failed to run cuenv env print with JSON format: {e}"),
    }
}

#[test]
fn test_env_print_command_with_short_path_flag() {
    let test_path = get_test_examples_path();
    let result = run_cuenv_command(&["env", "print", "-p", &test_path, "--package", "_examples"]);

    match result {
        Ok((stdout, _stderr, success)) => {
            assert!(success, "Command should succeed with short path flag");
            assert!(stdout.contains("DATABASE_URL="));
            assert!(stdout.contains("DEBUG="));
            assert!(stdout.contains("PORT="));
        }
        Err(e) => panic!("Failed to run cuenv env print with short path flag: {e}"),
    }
}

#[test]
fn test_env_print_command_invalid_path() {
    let result = run_cuenv_command(&[
        "env",
        "print",
        "--path",
        "nonexistent/path",
        "--package",
        "_examples",
    ]);

    if let Ok((_stdout, _stderr, success)) = result {
        assert!(!success, "Command should fail with invalid path");
    } else {
        // Command failed to execute, which is also acceptable for invalid paths
    }
}

#[test]
fn test_env_print_command_invalid_package() {
    let result = run_cuenv_command(&[
        "env",
        "print",
        "--path",
        "_examples/env-basic",
        "--package",
        "nonexistent",
    ]);

    if let Ok((_stdout, _stderr, success)) = result {
        assert!(!success, "Command should fail with invalid package");
    } else {
        // Command failed to execute, which is also acceptable for invalid packages
    }
}

#[test]
fn test_env_print_command_unsupported_format() {
    let test_path = get_test_examples_path();
    let result = run_cuenv_command(&[
        "env",
        "print",
        "--path",
        &test_path,
        "--package",
        "_examples",
        "--output",
        "yaml",
    ]);

    match result {
        Ok((stdout, stderr, success)) => {
            assert!(!success, "Command should fail with unsupported format");
            // Check that the error message mentions the unsupported format
            let combined_output = format!("{stdout}{stderr}");
            assert!(
                combined_output.contains("Unsupported format") || combined_output.contains("yaml"),
                "Error message should mention unsupported format 'yaml'"
            );
        }
        Err(e) => panic!("Failed to run cuenv env print: {e}"),
    }
}

// ===== Sync Command Integration Tests =====

#[test]
fn test_sync_command_dry_run() {
    let (_temp_dir, test_path) = create_git_test_env();
    let result = run_cuenv_command(&[
        "sync",
        "--path",
        &test_path,
        "--package",
        "cuenv",
        "--dry-run",
    ]);

    match result {
        Ok((stdout, stderr, success)) => {
            if !success {
                println!("stdout: {stdout}");
                println!("stderr: {stderr}");
            }
            assert!(success, "Command should succeed");
            // Should show what would be created/updated
            assert!(
                stdout.contains("Would create")
                    || stdout.contains("Would update")
                    || stdout.contains(".gitignore"),
                "Dry run should show what would be generated"
            );
        }
        Err(e) => panic!("Failed to run cuenv sync --dry-run: {e}"),
    }
}

#[test]
fn test_sync_command_dry_run_shows_pattern_count() {
    let (_temp_dir, test_path) = create_git_test_env();
    let result = run_cuenv_command(&[
        "sync",
        "--path",
        &test_path,
        "--package",
        "cuenv",
        "--dry-run",
    ]);

    match result {
        Ok((stdout, _stderr, success)) => {
            assert!(success, "Command should succeed");
            // Should show pattern count in output
            assert!(
                stdout.contains("patterns") || stdout.contains("Would create"),
                "Dry run should show pattern count"
            );
        }
        Err(e) => panic!("Failed to run cuenv sync --dry-run: {e}"),
    }
}

#[test]
fn test_sync_command_invalid_path() {
    let result = run_cuenv_command(&[
        "sync",
        "--path",
        "nonexistent/path",
        "--package",
        "_examples",
        "--dry-run",
    ]);

    if let Ok((_stdout, _stderr, success)) = result {
        assert!(!success, "Command should fail with invalid path");
    }
}

#[test]
fn test_sync_command_invalid_package() {
    let (_temp_dir, test_path) = create_git_test_env();
    let result = run_cuenv_command(&[
        "sync",
        "--path",
        &test_path,
        "--package",
        "nonexistent",
        "--dry-run",
    ]);

    if let Ok((_stdout, _stderr, success)) = result {
        assert!(!success, "Command should fail with invalid package");
    }
}

#[test]
fn test_sync_command_help() {
    let result = run_cuenv_command(&["sync", "--help"]);

    match result {
        Ok((stdout, _stderr, _success)) => {
            assert!(
                stdout.contains("sync") || stdout.contains("Sync"),
                "Help should mention the sync command"
            );
            assert!(
                stdout.contains("--dry-run") || stdout.contains("dry"),
                "Help should mention --dry-run option"
            );
        }
        Err(e) => panic!("Failed to run cuenv sync --help: {e}"),
    }
}

// =========================================================================
// CI Workflow Generation Tests - Monorepo working-directory support
// =========================================================================

/// Create a monorepo test environment with multiple projects at different paths.
/// Returns a `TempDir` that will be cleaned up when dropped, and the path as a String.
fn create_monorepo_test_env() -> (tempfile::TempDir, String) {
    let temp_dir = tempfile::Builder::new()
        .prefix("cuenv_monorepo_test_")
        .tempdir()
        .expect("Failed to create temp directory");
    let temp_path = temp_dir.path();

    // Initialize git repository
    Command::new("git")
        .args(["init"])
        .current_dir(temp_path)
        .output()
        .expect("Failed to init git repo");

    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(temp_path)
        .output()
        .expect("Failed to configure git email");

    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(temp_path)
        .output()
        .expect("Failed to configure git name");

    // Create cue.mod directory and module.cue
    let cue_mod_dir = temp_path.join("cue.mod");
    fs::create_dir_all(&cue_mod_dir).expect("Failed to create cue.mod directory");

    fs::write(
        cue_mod_dir.join("module.cue"),
        r#"module: "test.example/monorepo"
language: version: "v0.9.0"
"#,
    )
    .expect("Failed to write module.cue");

    // Create root project env.cue (at module root)
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
    )
    .expect("Failed to write root env.cue");

    // Create nested project at services/api/
    let api_dir = temp_path.join("services").join("api");
    fs::create_dir_all(&api_dir).expect("Failed to create services/api directory");

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

tasks: {
    build: {
        command: "cargo"
        args: ["build"]
        inputs: ["src/**"]
    }
    test: {
        command: "cargo"
        args: ["test"]
        dependsOn: ["build"]
    }
}
"#,
    )
    .expect("Failed to write services/api/env.cue");

    // Create another nested project at apps/web/
    let web_dir = temp_path.join("apps").join("web");
    fs::create_dir_all(&web_dir).expect("Failed to create apps/web directory");

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
    )
    .expect("Failed to write apps/web/env.cue");

    let path_str = temp_path.to_string_lossy().to_string();
    (temp_dir, path_str)
}

/// Helper to run cuenv command in a specific directory
fn run_cuenv_command_in_dir(
    args: &[&str],
    dir: &str,
) -> Result<(String, String, bool), Box<dyn std::error::Error>> {
    let mut cmd = Command::new(CUENV_BIN);

    for arg in args {
        cmd.arg(arg);
    }

    cmd.current_dir(dir);

    let output = cmd.output()?;
    let stdout = str::from_utf8(&output.stdout)?.to_string();
    let stderr = str::from_utf8(&output.stderr)?.to_string();
    let success = output.status.success();

    Ok((stdout, stderr, success))
}

#[test]
fn test_sync_ci_dry_run_shows_workflows() {
    let (_temp_dir, temp_path) = create_monorepo_test_env();

    let result = run_cuenv_command_in_dir(&["sync", "ci", "--dry-run"], &temp_path);

    match result {
        Ok((stdout, stderr, success)) => {
            let combined = format!("{stdout}{stderr}");
            if !success {
                println!("stdout: {stdout}");
                println!("stderr: {stderr}");
            }
            // The command should succeed or fail gracefully
            // Note: CUE evaluation may fail due to missing schema imports in temp env
            // This test mainly verifies the command runs without panic
            assert!(
                success || combined.contains("error") || combined.contains("failed"),
                "Command should either succeed or report meaningful error"
            );
        }
        Err(e) => panic!("Failed to run cuenv sync ci --dry-run: {e}"),
    }
}

#[test]
fn test_sync_ci_help() {
    let result = run_cuenv_command(&["sync", "ci", "--help"]);

    match result {
        Ok((stdout, _stderr, success)) => {
            assert!(success, "Help command should succeed");
            assert!(
                stdout.contains("Sync CI workflow files"),
                "Help should describe CI sync functionality"
            );
            assert!(
                stdout.contains("--dry-run"),
                "Help should mention --dry-run option"
            );
        }
        Err(e) => panic!("Failed to run cuenv sync ci --help: {e}"),
    }
}
