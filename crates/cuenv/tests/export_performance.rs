//! Performance regression tests for the export command.
//!
//! The export command is called on every shell prompt and must remain fast
//! (sub-10ms) for a good user experience. These tests ensure we don't regress.

use std::process::Command;
use std::time::Instant;
use tempfile::TempDir;

/// Export with no env.cue must complete in <10ms (strict threshold).
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

    assert!(output.status.success(), "Export failed: {:?}", output);

    // The output should be a no-op script that clears state
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("set -e") || stdout.contains("true"),
        "Expected no-op fish script, got: {}",
        stdout
    );

    assert!(
        elapsed.as_millis() < 10,
        "PERFORMANCE REGRESSION: Export took {}ms, expected <10ms for no-env-cue case",
        elapsed.as_millis()
    );
}

/// Export performance regression test - strict 10ms threshold.
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

    times.sort();
    let median = times[times.len() / 2];
    let min = times[0];

    // Strict 10ms threshold - this is critical for shell prompt performance
    assert!(
        median < 10,
        "PERFORMANCE REGRESSION: Median export time {}ms exceeds 10ms threshold.\n\
         Min: {}ms, All times: {:?}\n\
         Export must be sub-10ms for shell prompt integration.",
        median,
        min,
        times
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
            "Export failed for shell {}: {:?}",
            shell,
            output
        );

        assert!(
            elapsed.as_millis() < 10,
            "PERFORMANCE REGRESSION: Export for shell {} took {}ms, expected <10ms",
            shell,
            elapsed.as_millis()
        );
    }
}
