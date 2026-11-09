//! Integration tests for Go-Rust FFI bridge
//!
//! These tests focus on memory management, concurrency safety,
//! and proper resource cleanup across the FFI boundary.

use cuengine::CStringPtr;
use std::error::Error;
use std::ffi::CString;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use std::{fs, io, thread};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn create_module(root: &Path) -> TestResult {
    fs::create_dir_all(root.join("cue.mod"))?;
    fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"test.example/ffi@v0\"\nlanguage: version: \"v0.14.1\"\n",
    )?;
    Ok(())
}

/// Test that `CStringPtr` properly handles memory across FFI boundary
#[test]
#[allow(unsafe_code)]
fn test_cstring_ptr_raii_memory_management() -> TestResult {
    // Create multiple CStringPtr instances to test RAII
    let test_strings = vec!["test1", "test2", "test3", "longer test string", ""];

    for test_str in test_strings {
        let wrapper = c_allocated_cstring_ptr(test_str)?;

        // Use the string
        // SAFETY: the helper returns an owned, non-null C string copied from test_str.
        let converted = unsafe { wrapper.to_str()? };
        assert_eq!(converted, test_str);

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
    create_module(temp_dir.path())?;

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

                for call_id in 0..CALLS_PER_THREAD {
                    let json = cuengine::evaluate_cue_package(&temp_path, "cuenv").unwrap_or_else(
                        |error| panic!("thread {thread_id}, call {call_id}: {error}"),
                    );
                    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
                    assert_eq!(value["env"]["THREAD_TEST"], "concurrent_value");
                    assert_eq!(value["env"]["THREAD_ID"], 1);

                    // Small delay to increase chance of race conditions
                    thread::sleep(Duration::from_millis(1));
                }

                CALLS_PER_THREAD
            })
        })
        .collect();

    // Collect results from all threads
    let mut total_successes = 0;
    for handle in handles {
        total_successes += handle
            .join()
            .map_err(|_| io::Error::other("concurrent FFI worker thread panicked"))?;
    }
    assert_eq!(total_successes, NUM_THREADS * CALLS_PER_THREAD);

    Ok(())
}

/// Repeated evaluations must return the current input after each change.
#[test]
fn test_repeated_ffi_calls_return_current_data() -> TestResult {
    let temp_dir = TempDir::new()?;
    create_module(temp_dir.path())?;

    for iteration in 0..50 {
        let file_index = iteration % 3;
        let cue_content = format!(
            r#"package cuenv

env: {{
    LEAK_TEST: "value_{i}"
    DATA: "{}"
}}
"#,
            "x".repeat(100 * (file_index + 1)),
            i = file_index,
        );
        fs::write(temp_dir.path().join("env.cue"), cue_content)?;
        let json = cuengine::evaluate_cue_package(temp_dir.path(), "cuenv")?;
        let value: serde_json::Value = serde_json::from_str(&json)?;
        assert_eq!(value["env"]["LEAK_TEST"], format!("value_{file_index}"));
        assert_eq!(
            value["env"]["DATA"].as_str().unwrap().len(),
            100 * (file_index + 1)
        );
    }

    Ok(())
}

/// Test FFI error handling with various invalid inputs
#[test]
fn test_ffi_error_handling_edge_cases() -> TestResult {
    let temp_dir = TempDir::new()?;
    create_module(temp_dir.path())?;
    fs::write(
        temp_dir.path().join("env.cue"),
        "package cuenv\nenv: {TEST: \"value\"}",
    )?;

    // Test cases that should trigger different error paths
    let long_package_name = "x".repeat(1000);
    let default = cuengine::evaluate_cue_package(temp_dir.path(), "")?;
    let value: serde_json::Value = serde_json::from_str(&default)?;
    assert_eq!(value["env"]["TEST"], "value");
    for package_name in [
        &long_package_name[..],
        "package!@#$%",
        "definitely_not_a_real_package",
    ] {
        let error = cuengine::evaluate_cue_package(temp_dir.path(), package_name).unwrap_err();
        assert!(!error.to_string().is_empty());
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
    create_module(&nested_dir)?;

    // Create CUE file in nested location
    let cue_content = r#"package cuenv

env: {
    NESTED_TEST: "deep_value"
    DEPTH: 3
}
"#;
    fs::write(nested_dir.join("env.cue"), cue_content)?;

    // Test evaluating from nested directory
    let json = cuengine::evaluate_cue_package(&nested_dir, "cuenv")?;
    let value: serde_json::Value = serde_json::from_str(&json)?;
    assert_eq!(value["env"]["NESTED_TEST"], "deep_value");
    assert_eq!(value["env"]["DEPTH"], 3);

    // Test with directory containing spaces and unicode
    let unicode_dir = temp_dir.path().join("测试 directory with spaces");
    fs::create_dir_all(&unicode_dir)?;
    create_module(&unicode_dir)?;

    let unicode_cue = r#"package cuenv

env: {
    UNICODE_TEST: "unicode_value"
    PATH_TYPE: "unicode_with_spaces"
}
"#;
    fs::write(unicode_dir.join("env.cue"), unicode_cue)?;

    let json = cuengine::evaluate_cue_package(&unicode_dir, "cuenv")?;
    let value: serde_json::Value = serde_json::from_str(&json)?;
    assert_eq!(value["env"]["UNICODE_TEST"], "unicode_value");
    assert_eq!(value["env"]["PATH_TYPE"], "unicode_with_spaces");

    Ok(())
}

/// Test that FFI cleanup works correctly even when errors occur
#[test]
fn test_ffi_cleanup_on_errors() -> TestResult {
    let temp_dir = TempDir::new()?;
    create_module(temp_dir.path())?;

    // Create various files that might cause different types of errors
    let invalid_cue_files = vec![
        (
            "syntax_error.cue",
            "package cuenv\n\nthis is not valid CUE {",
        ),
        ("empty.cue", ""), // Empty file
        ("wrong_package.cue", "package wrong\nenv: {TEST: \"value\"}"),
        ("conflict.cue", "package cuenv\nenv: {A: 1 & 2}"),
    ];

    for (filename, content) in invalid_cue_files {
        fs::write(temp_dir.path().join(filename), content)?;

        let error = cuengine::evaluate_cue_package(temp_dir.path(), "cuenv").unwrap_err();
        assert!(
            !error.to_string().is_empty(),
            "File {filename} returned an empty error"
        );

        // Clean up
        fs::remove_file(temp_dir.path().join(filename))?;
    }

    // This lenient evaluator exports unresolved cycles as nulls.
    let circular = temp_dir.path().join("circular.cue");
    fs::write(&circular, "package cuenv\nenv: {A: env.B, B: env.A}")?;
    let json = cuengine::evaluate_cue_package(temp_dir.path(), "cuenv")?;
    let value: serde_json::Value = serde_json::from_str(&json)?;
    assert!(value["env"]["A"].is_null());
    assert!(value["env"]["B"].is_null());
    fs::remove_file(circular)?;

    // After all error cases, verify normal operation still works
    let valid_cue = "package cuenv\nenv: {RECOVERY_TEST: \"recovered\"}";
    fs::write(temp_dir.path().join("env.cue"), valid_cue)?;

    let json = cuengine::evaluate_cue_package(temp_dir.path(), "cuenv")?;
    let value: serde_json::Value = serde_json::from_str(&json)?;
    assert_eq!(value["env"]["RECOVERY_TEST"], "recovered");

    Ok(())
}

/// Test FFI performance characteristics
#[test]
fn test_ffi_performance_characteristics() -> TestResult {
    let temp_dir = TempDir::new()?;
    create_module(temp_dir.path())?;

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
    for _ in 0..10 {
        let start = Instant::now();
        let json = cuengine::evaluate_cue_package(temp_dir.path(), "cuenv")?;
        times.push(start.elapsed());
        let value: serde_json::Value = serde_json::from_str(&json)?;
        assert_eq!(value["env"]["PERF_TEST"], "performance_test");
        assert!(
            value["env"]["LARGE_DATA"]
                .as_str()
                .unwrap()
                .contains("Lorem ipsum")
        );
        assert_eq!(
            value["env"]["NESTED"]["LEVEL1"]["LEVEL2"]["LEVEL3"],
            "deep_value"
        );
    }
    let sample_count = u32::try_from(times.len())?;
    assert_eq!(sample_count, 10);
    let avg_time = times.iter().sum::<Duration>() / sample_count;
    let max_time = times
        .iter()
        .max()
        .ok_or_else(|| io::Error::other("missing performance samples"))?;
    assert!(
        max_time < &Duration::from_secs(5),
        "No single call should take longer than 5 seconds"
    );
    assert!(
        avg_time < Duration::from_secs(1),
        "Average call time should be under 1 second"
    );

    Ok(())
}
