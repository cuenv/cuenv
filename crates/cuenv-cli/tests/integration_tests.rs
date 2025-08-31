//! Integration tests for the cuenv CLI
//! 
//! These tests exercise the complete CLI functionality, including
//! argument parsing, command execution, and output formatting.

use std::process::Command;
use std::str;

/// Test helper to run cuenv CLI commands
fn run_cuenv_command(args: &[&str]) -> Result<(String, String, bool), Box<dyn std::error::Error>> {
    let mut cmd = Command::new("cargo");
    cmd.arg("run")
        .arg("--bin")
        .arg("cuenv")
        .arg("--");
    
    for arg in args {
        cmd.arg(arg);
    }
    
    let output = cmd.output()?;
    let stdout = str::from_utf8(&output.stdout)?.to_string();
    let stderr = str::from_utf8(&output.stderr)?.to_string();
    let success = output.status.success();
    
    Ok((stdout, stderr, success))
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
        "--level", "info", 
        "--json", 
        "--format", "structured",
        "version"
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
            
            assert_eq!(lines1.len(), lines2.len(), "Output should have same number of lines");
            
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