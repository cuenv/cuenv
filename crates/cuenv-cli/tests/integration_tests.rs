//! Integration tests for the cuenv CLI
//!
//! These tests exercise the complete CLI functionality, including
//! argument parsing, command execution, and output formatting.

use std::process::Command;
use std::str;

/// Test helper to run cuenv CLI commands
fn run_cuenv_command(args: &[&str]) -> Result<(String, String, bool), Box<dyn std::error::Error>> {
    let mut cmd = Command::new("cargo");
    cmd.arg("run").arg("--bin").arg("cuenv").arg("--");

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
        .join("examples/env-basic")
        .to_string_lossy()
        .to_string()
}

#[test]
fn test_version_command_basic() {
    let result = run_cuenv_command(&["version"]);

    match result {
        Ok((stdout, _stderr, success)) => {
            assert!(success, "Command should succeed");
            assert!(stdout.contains("cuenv-cli"));
            assert!(stdout.contains("0.1.0"));
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
            assert!(stdout.contains("cuenv-cli"));
            assert!(stdout.contains("0.1.0"));

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
            assert!(stdout.contains("cuenv-cli"));
            assert!(stdout.contains("0.1.0"));

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
            assert!(stdout.contains("cuenv-cli"));

            // stderr should contain JSON formatted logs
            if !stderr.is_empty() {
                // If there are logs, they should be in JSON format
                assert!(stderr.contains('{') && stderr.contains('}'));
            }
        }
        Err(e) => panic!("Failed to run cuenv version with JSON flag: {e}"),
    }
}

#[test]
fn test_version_command_short_level_flag() {
    let result = run_cuenv_command(&["-l", "warn", "version"]);

    match result {
        Ok((stdout, _stderr, success)) => {
            assert!(success, "Command should succeed");
            assert!(stdout.contains("cuenv-cli"));
            assert!(stdout.contains("0.1.0"));
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
            assert!(stdout.contains("--level") || stdout.contains("-l"));
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
    let result = run_cuenv_command(&[
        "--level",
        "info",
        "--json",
        "--format",
        "structured",
        "version",
    ]);

    match result {
        Ok((stdout, _stderr, success)) => {
            assert!(success, "Command should succeed with combined flags");
            assert!(stdout.contains("cuenv-cli"));
            assert!(stdout.contains("0.1.0"));
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
        "examples",
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
        "examples",
        "--format",
        "json",
    ]);

    match result {
        Ok((stdout, _stderr, success)) => {
            assert!(success, "Command should succeed");

            // Parse as JSON to verify it's valid JSON
            let parsed: serde_json::Value =
                serde_json::from_str(&stdout).expect("Output should be valid JSON");

            assert_eq!(parsed["DATABASE_URL"], "postgres://localhost/mydb");
            assert_eq!(parsed["DEBUG"], true);
            assert_eq!(parsed["PORT"], 3000);
            assert_eq!(parsed["BASE_URL"], "https://api.example.com");
            assert_eq!(parsed["API_ENDPOINT"], "https://api.example.com/v1");
        }
        Err(e) => panic!("Failed to run cuenv env print with JSON format: {e}"),
    }
}

#[test]
fn test_env_print_command_with_short_path_flag() {
    let test_path = get_test_examples_path();
    let result = run_cuenv_command(&["env", "print", "-p", &test_path, "--package", "examples"]);

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
        "examples",
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
        "examples/env-basic",
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
        "examples",
        "--format",
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
