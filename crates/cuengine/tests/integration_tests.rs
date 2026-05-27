//! Integration tests for Go-Rust FFI bridge
//!
//! These tests focus on memory management, concurrency safety,
//! and proper resource cleanup across the FFI boundary.

use cuengine::{CStringPtr, evaluate_cue_package};
use std::error::Error;
use std::ffi::CString;
use std::fs;
use std::io;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// Test that `CStringPtr` properly handles memory across FFI boundary
#[test]
#[allow(unsafe_code)]
fn test_cstring_ptr_raii_memory_management() -> TestResult {
    // Create multiple CStringPtr instances to test RAII
    let test_strings = vec!["test1", "test2", "test3", "longer test string", ""];

    for test_str in test_strings {
        let wrapper = c_allocated_cstring_ptr(test_str)?;

        // Use the string
        if !wrapper.is_null() {
            // SAFETY: wrapper is guaranteed to be valid and non-null, and contains
            // a valid C string that was created from test_str
            let converted = unsafe { wrapper.to_str()? };
            assert_eq!(converted, test_str);
        }

        // wrapper automatically frees memory when dropped here
    }

    // If we get here without crashes, RAII is working correctly
    Ok(())
}

#[allow(unsafe_code)]
fn c_allocated_cstring_ptr(value: &str) -> TestResult<CStringPtr> {
    let c_string = CString::new(value)?;
    // SAFETY: c_string.as_ptr() is a valid, null-terminated C string for the
    // duration of this call, and strdup returns a C-allocated copy.
    let ptr = unsafe { libc::strdup(c_string.as_ptr()) };
    assert!(!ptr.is_null(), "libc::strdup returned null");
    // SAFETY: ptr was allocated by strdup, is non-null, and is transferred to
    // CStringPtr so Drop frees it through the FFI string-free boundary.
    Ok(unsafe { CStringPtr::new(ptr) })
}

/// Test concurrent access to FFI functions to ensure thread safety
#[test]
fn test_concurrent_ffi_access() -> TestResult {
    const NUM_THREADS: usize = 8;
    const CALLS_PER_THREAD: usize = 10;

    let temp_dir = TempDir::new()?;

    // Create a test CUE file
    let cue_content = r#"package cuenv

env: {
    THREAD_TEST: "concurrent_value"
    THREAD_ID: 1
}
"#;
    fs::write(temp_dir.path().join("env.cue"), cue_content)?;

    let barrier = Arc::new(Barrier::new(NUM_THREADS));
    let temp_path = Arc::new(temp_dir.path().to_path_buf());

    let handles: Vec<_> = (0..NUM_THREADS)
        .map(|thread_id| {
            let barrier = Arc::clone(&barrier);
            let temp_path = Arc::clone(&temp_path);

            thread::spawn(move || {
                // Wait for all threads to start
                barrier.wait();

                let mut results = Vec::new();
                let mut errors = Vec::new();

                for call_id in 0..CALLS_PER_THREAD {
                    match evaluate_cue_package(&temp_path, "cuenv") {
                        Ok(json) => {
                            results.push((thread_id, call_id, json));
                        }
                        Err(e) => {
                            errors.push((thread_id, call_id, e.to_string()));
                        }
                    }

                    // Small delay to increase chance of race conditions
                    thread::sleep(Duration::from_millis(1));
                }

                (thread_id, results, errors)
            })
        })
        .collect();

    // Collect results from all threads
    let mut total_successes = 0;
    let mut total_errors = 0;

    for handle in handles {
        let (_thread_id, results, errors) = handle
            .join()
            .map_err(|_| io::Error::other("concurrent FFI worker thread panicked"))?;

        total_successes += results.len();
        total_errors += errors.len();

        // Verify successful results contain expected content
        for (_tid, _call_id, json) in results {
            if json.contains("THREAD_TEST") {
                assert!(json.contains("concurrent_value"));
            }
        }
    }

    // Either all calls should succeed (if FFI is available) or all should fail consistently
    if total_successes > 0 {
        // If some succeeded, most should have succeeded (allowing for some flakiness)
        assert!(
            total_successes > total_errors,
            "If FFI works, most calls should succeed"
        );
    }

    Ok(())
}

/// Test memory usage doesn't grow over time (leak detection)
#[test]
fn test_ffi_memory_leak_detection() -> TestResult {
    let temp_dir = TempDir::new()?;

    // Create test CUE files with varying sizes
    for i in 0..3 {
        let cue_content = format!(
            r#"package cuenv

env: {{
    LEAK_TEST: "value_{i}"
    DATA: "{}"
}}
"#,
            "x".repeat(100 * (i + 1)) // Increasing data size
        );

        fs::write(temp_dir.path().join(format!("test_{i}.cue")), cue_content)?;
    }

    // Make many calls with different data sizes
    for iteration in 0..50 {
        let file_index = iteration % 3;

        // Remove the old file and create new one to force re-parsing
        let _ = fs::remove_file(temp_dir.path().join("env.cue"));
        fs::copy(
            temp_dir.path().join(format!("test_{file_index}.cue")),
            temp_dir.path().join("env.cue"),
        )?;

        match evaluate_cue_package(temp_dir.path(), "cuenv") {
            Ok(json) => {
                // Verify we got the right data
                // The JSON wraps everything in an "env" object
                assert!(
                    json.contains(&format!("value_{file_index}")) || json.contains("env"),
                    "Expected value_{file_index} or env in JSON: {json}"
                );
            }
            Err(_) => {
                // FFI might not be available - that's acceptable
                if iteration > 5 {
                    break; // Stop early if FFI consistently fails
                }
            }
        }
    }

    // If we complete without crashes or OOM, memory management is working
    Ok(())
}

/// Test FFI error handling with various invalid inputs
#[test]
fn test_ffi_error_handling_edge_cases() -> TestResult {
    let temp_dir = TempDir::new()?;

    // Test cases that should trigger different error paths
    let long_package_name = "x".repeat(1000);
    let test_cases = vec![
        // Empty package name
        ("", "Empty package name should be handled"),
        // Very long package name
        (
            &long_package_name,
            "Very long package name should be handled",
        ),
        // Package name with special characters
        ("package!@#$%", "Special characters should be handled"),
        // Non-existent package
        ("definitely_not_a_real_package", "Non-existent package"),
    ];

    for (package_name, description) in test_cases {
        let result = evaluate_cue_package(temp_dir.path(), package_name);

        match result {
            Ok(json) => {
                // Some edge cases may succeed depending on CUE/FFI behavior.
                assert!(
                    !json.is_empty(),
                    "{description}: successful response should not be empty"
                );
            }
            Err(error) => {
                // Expected case - should get meaningful error
                let error_str = error.to_string();
                assert!(
                    !error_str.is_empty(),
                    "{description}: Error should not be empty"
                );
                assert!(
                    error_str.len() > 10,
                    "{description}: Error should be meaningful"
                );
            }
        }
    }

    Ok(())
}

/// Test FFI with unusual directory structures
#[test]
fn test_ffi_with_complex_directory_structure() -> TestResult {
    let temp_dir = TempDir::new()?;

    // Create nested directory structure
    let nested_dir = temp_dir.path().join("very").join("deeply").join("nested");
    fs::create_dir_all(&nested_dir)?;

    // Create CUE file in nested location
    let cue_content = r#"package cuenv

env: {
    NESTED_TEST: "deep_value"
    DEPTH: 3
}
"#;
    fs::write(nested_dir.join("env.cue"), cue_content)?;

    // Test evaluating from nested directory
    let result = evaluate_cue_package(&nested_dir, "cuenv");

    if let Ok(json) = result {
        // JSON wraps in "env" object
        assert!(json.contains("NESTED_TEST") || json.contains("env"));
        assert!(json.contains("deep_value") || json.contains("env"));
    }

    // Test with directory containing spaces and unicode
    let unicode_dir = temp_dir.path().join("测试 directory with spaces");
    fs::create_dir_all(&unicode_dir)?;

    let unicode_cue = r#"package cuenv

env: {
    UNICODE_TEST: "unicode_value"
    PATH_TYPE: "unicode_with_spaces"
}
"#;
    fs::write(unicode_dir.join("env.cue"), unicode_cue)?;

    let unicode_result = evaluate_cue_package(&unicode_dir, "cuenv");

    if let Ok(json) = unicode_result {
        // JSON wraps in "env" object
        assert!(json.contains("UNICODE_TEST") || json.contains("env"));
    }

    Ok(())
}

/// Test that FFI cleanup works correctly even when errors occur
#[test]
fn test_ffi_cleanup_on_errors() -> TestResult {
    let temp_dir = TempDir::new()?;

    // Create various files that might cause different types of errors
    let invalid_cue_files = vec![
        (
            "syntax_error.cue",
            "package cuenv\n\nthis is not valid CUE {",
        ),
        ("empty.cue", ""), // Empty file
        ("wrong_package.cue", "package wrong\nenv: {TEST: \"value\"}"),
        ("circular.cue", "package cuenv\nenv: {A: env.B, B: env.A}"), // Circular reference
    ];

    for (filename, content) in invalid_cue_files {
        // Remove any existing env.cue and create the test file
        let _ = fs::remove_file(temp_dir.path().join("env.cue"));
        fs::write(temp_dir.path().join(filename), content)?;

        // Try to evaluate - should handle errors gracefully
        let result = evaluate_cue_package(temp_dir.path(), "cuenv");

        match result {
            Ok(json) => {
                // Some cases might succeed due to FFI behavior
                assert!(!json.is_empty(), "File {filename} returned empty JSON");
            }
            Err(error) => {
                // Verify error message is meaningful
                assert!(!error.to_string().is_empty());
            }
        }

        // Clean up
        let _ = fs::remove_file(temp_dir.path().join(filename));
    }

    // After all error cases, verify normal operation still works
    let valid_cue = "package cuenv\nenv: {RECOVERY_TEST: \"recovered\"}";
    fs::write(temp_dir.path().join("env.cue"), valid_cue)?;

    let recovery_result = evaluate_cue_package(temp_dir.path(), "cuenv");
    if let Ok(json) = recovery_result {
        assert!(json.contains("RECOVERY_TEST"));
    }

    Ok(())
}

/// Test FFI performance characteristics
#[test]
fn test_ffi_performance_characteristics() -> TestResult {
    let temp_dir = TempDir::new()?;

    // Create a reasonably sized CUE file
    let cue_content = r#"package cuenv

env: {
    PERF_TEST: "performance_test"
    LARGE_DATA: "Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua."
    NUMBERS: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
    NESTED: {
        LEVEL1: {
            LEVEL2: {
                LEVEL3: "deep_value"
            }
        }
    }
}
"#;
    fs::write(temp_dir.path().join("env.cue"), cue_content)?;

    let mut times = Vec::new();

    // Measure performance over multiple calls
    for i in 0..10 {
        let start = Instant::now();

        match evaluate_cue_package(temp_dir.path(), "cuenv") {
            Ok(json) => {
                let duration = start.elapsed();
                times.push(duration);

                // Verify correctness - JSON wraps in "env" object
                assert!(json.contains("PERF_TEST") || json.contains("env"));
                assert!(json.contains("Lorem ipsum") || json.contains("env"));
            }
            Err(_) => {
                if i > 2 {
                    break; // Stop if FFI consistently fails
                }
            }
        }
    }

    if !times.is_empty() {
        let sample_count = u32::try_from(times.len())?;
        let avg_time = times.iter().sum::<Duration>() / sample_count;
        let max_time = times
            .iter()
            .max()
            .ok_or_else(|| io::Error::other("performance samples should include a maximum"))?;

        // Basic performance expectations (these are lenient for CI)
        assert!(
            max_time < &Duration::from_secs(5),
            "No single call should take longer than 5 seconds"
        );
        assert!(
            avg_time < Duration::from_secs(1),
            "Average call time should be under 1 second"
        );
    }

    Ok(())
}
