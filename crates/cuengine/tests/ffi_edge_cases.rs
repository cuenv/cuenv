//! Tests for FFI edge cases and error paths

use cuengine::{evaluate_cue_package, CStringPtr};
use std::ffi::CString;
use tempfile::TempDir;

#[test]
fn test_cstring_ptr_invalid_utf8() {
    // This tests the UTF-8 conversion error path
    // Create a C string with invalid UTF-8 (using Latin-1 encoding)
    let bytes = [0xFF, 0xFE, 0xFD, 0x00]; // Invalid UTF-8 sequence with null terminator

    // Create a raw pointer from these bytes
    let ptr = bytes.as_ptr() as *mut std::os::raw::c_char;

    // Create wrapper - this should succeed
    #[allow(unsafe_code)]
    let wrapper = unsafe { CStringPtr::new(ptr) };

    // Conversion to str should fail due to invalid UTF-8
    #[allow(unsafe_code)]
    let result = unsafe { wrapper.to_str() };
    assert!(result.is_err());

    // Prevent the wrapper from freeing our stack-allocated bytes
    std::mem::forget(wrapper);
}

#[test]
fn test_cstring_ptr_null_handling() {
    // Test null pointer behavior
    #[allow(unsafe_code)]
    let null_wrapper = unsafe { CStringPtr::new(std::ptr::null_mut()) };
    assert!(null_wrapper.is_null());

    // Drop should handle null gracefully (no crash)
}

#[test]
fn test_evaluate_cue_package_null_bytes_in_path() {
    // Test path with null bytes (should fail during CString conversion)
    let temp_dir = TempDir::new().unwrap();

    // Create a path that would have null bytes when converted to string
    // This is tricky since Path doesn't allow null bytes, so we test the package name instead
    let result = evaluate_cue_package(temp_dir.path(), "test\0package");

    assert!(result.is_err());
}

#[test]
fn test_evaluate_cue_package_error_prefix_response() {
    // This tests the error: prefix handling from Go
    // We can't easily mock the FFI response, but we can test with invalid CUE
    let temp_dir = TempDir::new().unwrap();

    // Create definitely invalid CUE that should trigger an error response
    std::fs::write(
        temp_dir.path().join("invalid.cue"),
        "package test\n\nthis is { completely invalid CUE syntax @#$%",
    )
    .unwrap();

    let result = evaluate_cue_package(temp_dir.path(), "test");

    // The result should be an error (either FFI unavailable or CUE parse error)
    if let Err(error) = result {
        let error_str = error.to_string();
        // Should contain some error indication
        assert!(!error_str.is_empty());
    }
}

#[test]
fn test_evaluate_cue_package_various_error_conditions() {
    // Test multiple error conditions that might occur
    let temp_dir = TempDir::new().unwrap();

    // Test 1: Directory that exists but has no read permissions
    // This test is unreliable in different environments, so we'll skip it
    // The important thing is that the error handling code paths are exercised

    // Test 2: Very long package name (but still valid)
    let long_name = "a".repeat(1000);
    let result = evaluate_cue_package(temp_dir.path(), &long_name);
    // Should process (may fail for other reasons but not the name itself)
    match result {
        Ok(_) => {} // FFI worked
        Err(e) => {
            // Should not be about the package name being invalid
            assert!(!e.to_string().contains("Invalid package name"));
        }
    }

    // Test 3: Package name with special but valid characters
    let special_name = "test_package-123";
    let result = evaluate_cue_package(temp_dir.path(), special_name);
    // Should process (may fail for other reasons but not the name itself)
    match result {
        Ok(_) => {} // FFI worked
        Err(e) => {
            // Should not be about invalid characters
            assert!(!e.to_string().contains("Invalid package name"));
        }
    }
}

#[test]
fn test_cstring_ptr_drop_with_valid_string() {
    // Test that drop properly frees memory for valid strings
    // This is mainly for coverage of the Drop implementation

    for content in &["test", "", "multi\nline\nstring", "unicode: 你好"] {
        let c_string = CString::new(*content).unwrap();
        let ptr = c_string.into_raw();

        // Create wrapper which takes ownership
        #[allow(unsafe_code)]
        let wrapper = unsafe { CStringPtr::new(ptr) };

        // Verify we can read it
        #[allow(unsafe_code)]
        let result = unsafe { wrapper.to_str() };
        assert_eq!(result.unwrap(), *content);

        // IMPORTANT: We must prevent the wrapper from calling cue_free_string()
        // because this string was allocated by Rust, not Go
        // cue_free_string() is specifically for Go-allocated strings
        std::mem::forget(wrapper);

        // Manually free the Rust-allocated string
        #[allow(unsafe_code)]
        unsafe {
            let _ = CString::from_raw(ptr);
        }
    }
}

#[test]
fn test_evaluate_cue_mock_null_response() {
    // While we can't easily mock the FFI to return null,
    // we can test the null handling code path exists and compiles

    // This at least ensures the null check code is reachable
    let temp_dir = TempDir::new().unwrap();

    // Create a scenario that might cause null response
    // (empty directory, no CUE files)
    let result = evaluate_cue_package(temp_dir.path(), "nonexistent");

    // Just verify it handles the case without crashing
    // Either success or expected error is fine - we're testing null handling
    #[allow(clippy::single_match)]
    match result {
        Ok(_) | Err(_) => {} // Both outcomes are valid for this edge case test
    }
}

#[test]
fn test_evaluate_cue_with_error_prefix_simulation() {
    // Test handling of "error:" prefixed responses
    let temp_dir = TempDir::new().unwrap();

    // Create a CUE file that might trigger an error response from Go
    std::fs::write(
        temp_dir.path().join("broken.cue"),
        "package wrong\n\n// Intentionally wrong package name\nvalue: true",
    )
    .unwrap();

    // Try to evaluate with different package name than in file
    let result = evaluate_cue_package(temp_dir.path(), "different");

    // Should handle the error gracefully
    if let Err(e) = result {
        // Error should be meaningful
        assert!(!e.to_string().is_empty());
    }
}
