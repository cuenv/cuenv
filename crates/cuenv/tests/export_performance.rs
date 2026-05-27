//! Performance regression tests for the export command.
//!
//! The export command is called on every shell prompt and must remain fast
//! for a good user experience. These tests ensure we don't regress.
//!
//! Note: Threshold is 100ms to account for CI/sandbox/coverage instrumentation variability.
//! In practice, export should complete in <25ms on fast systems.

use std::error::Error;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;
use std::process::Output;
use std::time::Instant;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const CUENV_BIN: &str = env!("CARGO_BIN_EXE_cuenv");

/// Create a Command with a clean environment (no CI vars leaking).
fn clean_environment_command(bin: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("USER", std::env::var("USER").unwrap_or_default());
    cmd
}

fn run_export(dir: &Path, shell: &str) -> std::io::Result<Output> {
    clean_environment_command(CUENV_BIN)
        .args(["export", "--shell", shell, "--package", "cuenv"])
        .current_dir(dir)
        .output()
}

fn warm_export(dir: &Path, shell: &str) {
    let _ = run_export(dir, shell);
}

/// Export with no env.cue must complete quickly.
///
/// This is the fastest possible path - no CUE evaluation, no state checks.
/// Threshold is 100ms to account for coverage instrumentation overhead.
#[test]
fn test_export_no_env_cue_fast() -> TestResult {
    let temp_dir = TempDir::new()?;

    // Warm up (first run may be slower due to process/disk cache)
    warm_export(temp_dir.path(), "fish");

    let start = Instant::now();
    let output = run_export(temp_dir.path(), "fish")?;
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
        elapsed_ms < 100,
        "PERFORMANCE REGRESSION: Export took {elapsed_ms}ms, expected <100ms for no-env-cue case"
    );

    Ok(())
}

/// Export performance regression test - 100ms threshold.
///
/// Runs multiple iterations to get reliable timing and checks the median.
/// Threshold is relaxed to account for coverage instrumentation overhead.
#[test]
fn test_export_performance_threshold() -> TestResult {
    let temp_dir = TempDir::new()?;

    // Warm up run
    warm_export(temp_dir.path(), "fish");

    // Run 10 times and collect timings
    let mut times = Vec::new();
    for _ in 0..10 {
        let start = Instant::now();
        let _ = run_export(temp_dir.path(), "fish")?;
        times.push(start.elapsed().as_millis());
    }

    times.sort_unstable();
    let median = times[times.len() / 2];
    let min = times[0];

    // 100ms threshold - accounts for CI/sandbox/coverage variability while catching regressions
    assert!(
        median < 100,
        "PERFORMANCE REGRESSION: Median export time {median}ms exceeds 100ms threshold.\n\
         Min: {min}ms, All times: {times:?}\n\
         Export must be fast for shell prompt integration."
    );

    Ok(())
}

/// Test export with different shell types to ensure consistent performance.
#[test]
fn test_export_all_shells_fast() -> TestResult {
    let temp_dir = TempDir::new()?;
    let shells = ["fish", "bash", "zsh", "powershell"];

    for shell in shells {
        // Warm up
        warm_export(temp_dir.path(), shell);

        let start = Instant::now();
        let output = run_export(temp_dir.path(), shell)?;
        let elapsed = start.elapsed();

        assert!(
            output.status.success(),
            "Export failed for shell {shell}: {output:?}"
        );

        let elapsed_ms = elapsed.as_millis();
        assert!(
            elapsed_ms < 100,
            "PERFORMANCE REGRESSION: Export for shell {shell} took {elapsed_ms}ms, expected <100ms"
        );
    }

    Ok(())
}
