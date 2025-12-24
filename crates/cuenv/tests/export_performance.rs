//! Performance regression tests for the export command.
//!
//! The export command is called on every shell prompt and must remain fast
//! (sub-15ms) for a good user experience. These tests ensure we don't regress.
//!
//! Note: Threshold is 15ms instead of 10ms to account for CI/sandbox variability.

// Integration tests can use unwrap/expect for cleaner assertions
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;
use std::time::Instant;
use tempfile::TempDir;

/// Export with no env.cue must complete in <15ms.
///
/// This is the fastest possible path - no CUE evaluation, no state checks.
#[test]
fn test_export_no_env_cue_fast() {
    let temp_dir = TempDir::new().unwrap();

    // Warm up (first run may be slower due to process/disk cache)
    let _ = Command::new(env!("CARGO_BIN_EXE_cuenv"))
        .args(["export", "--shell", "fish", "--package", "cuenv"])
        .current_dir(temp_dir.path())
        .output();

    let start = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_cuenv"))
        .args(["export", "--shell", "fish", "--package", "cuenv"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to run cuenv");
    let elapsed = start.elapsed();

    assert!(output.status.success(), "Export failed: {output:?}");

    // The output should be a no-op script that clears state
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("set -e") || stdout.contains("true"),
        "Expected no-op fish script, got: {stdout}"
    );

    let elapsed_ms = elapsed.as_millis();
    assert!(
        elapsed_ms < 15,
        "PERFORMANCE REGRESSION: Export took {elapsed_ms}ms, expected <15ms for no-env-cue case"
    );
}

/// Export performance regression test - 15ms threshold.
///
/// Runs multiple iterations to get reliable timing and checks the median.
#[test]
fn test_export_performance_threshold() {
    let temp_dir = TempDir::new().unwrap();

    // Warm up run
    let _ = Command::new(env!("CARGO_BIN_EXE_cuenv"))
        .args(["export", "--shell", "fish", "--package", "cuenv"])
        .current_dir(temp_dir.path())
        .output();

    // Run 10 times and collect timings
    let mut times = Vec::new();
    for _ in 0..10 {
        let start = Instant::now();
        let _ = Command::new(env!("CARGO_BIN_EXE_cuenv"))
            .args(["export", "--shell", "fish", "--package", "cuenv"])
            .current_dir(temp_dir.path())
            .output()
            .expect("Failed to run cuenv");
        times.push(start.elapsed().as_millis());
    }

    times.sort_unstable();
    let median = times[times.len() / 2];
    let min = times[0];

    // 15ms threshold - accounts for CI/sandbox variability while catching regressions
    assert!(
        median < 15,
        "PERFORMANCE REGRESSION: Median export time {median}ms exceeds 15ms threshold.\n\
         Min: {min}ms, All times: {times:?}\n\
         Export must be sub-15ms for shell prompt integration."
    );
}

/// Test export with different shell types to ensure consistent performance.
#[test]
fn test_export_all_shells_fast() {
    let temp_dir = TempDir::new().unwrap();
    let shells = ["fish", "bash", "zsh", "powershell"];

    for shell in shells {
        // Warm up
        let _ = Command::new(env!("CARGO_BIN_EXE_cuenv"))
            .args(["export", "--shell", shell, "--package", "cuenv"])
            .current_dir(temp_dir.path())
            .output();

        let start = Instant::now();
        let output = Command::new(env!("CARGO_BIN_EXE_cuenv"))
            .args(["export", "--shell", shell, "--package", "cuenv"])
            .current_dir(temp_dir.path())
            .output()
            .expect("Failed to run cuenv");
        let elapsed = start.elapsed();

        assert!(
            output.status.success(),
            "Export failed for shell {shell}: {output:?}"
        );

        let elapsed_ms = elapsed.as_millis();
        assert!(
            elapsed_ms < 15,
            "PERFORMANCE REGRESSION: Export for shell {shell} took {elapsed_ms}ms, expected <15ms"
        );
    }
}
