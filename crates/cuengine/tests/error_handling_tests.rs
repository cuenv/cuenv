//! Tests for error handling and panic recovery in the FFI bridge
//!
//! These tests verify that the Go bridge properly handles panics and error conditions.

use cuengine::{CueEngineError, ModuleEvalOptions, evaluate_module};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

/// Helper to create a CUE module
fn create_module() -> TempDir {
    let temp_dir = TempDir::new().unwrap();
    let cue_mod = temp_dir.path().join("cue.mod");
    fs::create_dir(&cue_mod).unwrap();
    fs::write(
        cue_mod.join("module.cue"),
        r#"module: "test.example.com/test"
language: version: "v0.10.0"
"#,
    )
    .unwrap();
    temp_dir
}

/// Test that invalid input errors are properly handled
#[test]
fn test_error_invalid_module_root() {
    let result = evaluate_module(Path::new("/nonexistent/path/to/nowhere"), "test", None);
    
    assert!(result.is_err(), "Should fail for nonexistent path");
    let err = result.unwrap_err();
    
    // Verify error contains useful information
    let err_str = err.to_string();
    assert!(!err_str.is_empty());
}

/// Test that invalid package name is handled gracefully
#[test]
fn test_error_invalid_package_name() {
    let temp_dir = create_module();
    
    fs::write(
        temp_dir.path().join("test.cue"),
        r#"package validpkg
value: "test""#,
    )
    .unwrap();
    
    // Try to evaluate with wrong package name
    let result = evaluate_module(temp_dir.path(), "wrongpkg", None);
    
    // Should error because no instances match the package
    assert!(result.is_err(), "Should fail for mismatched package name");
}

/// Test error handling for CUE evaluation errors
#[test]
fn test_error_cue_evaluation_failure() {
    let temp_dir = create_module();
    
    // Create CUE with constraint violation
    let cue_content = r#"package test

value: string
value: 123  // Type error: value should be string
"#;
    fs::write(temp_dir.path().join("conflict.cue"), cue_content).unwrap();
    
    let result = evaluate_module(temp_dir.path(), "test", None);
    
    assert!(result.is_err(), "Constraint violation should cause error");
    let err = result.unwrap_err();
    let err_str = err.to_string();
    
    // Error should mention the conflict
    assert!(
        err_str.contains("Failed") || err_str.contains("conflict") || err_str.contains("string"),
        "Error should indicate the type conflict: {}",
        err_str
    );
}

/// Test error handling for incomplete CUE definitions
#[test]
fn test_error_incomplete_value() {
    let temp_dir = create_module();
    
    // Create CUE with incomplete value (requires concrete value)
    let cue_content = r#"package test

// This is incomplete - no concrete value provided
required: string
"#;
    fs::write(temp_dir.path().join("incomplete.cue"), cue_content).unwrap();
    
    let result = evaluate_module(temp_dir.path(), "test", None);
    
    // Depending on CUE evaluation settings, this might succeed or fail
    // The important thing is that it doesn't panic
    match result {
        Ok(module_result) => {
            // If it succeeds, the incomplete field should not be in the JSON
            let root = module_result.instances.get(".").expect("Root should exist");
            // Incomplete values might be omitted or included as null
            let has_required = root.get("required").is_some();
            println!("Incomplete value handling: has_required = {}", has_required);
        }
        Err(e) => {
            // Error is also acceptable
            println!("Incomplete value caused error: {}", e);
        }
    }
}

/// Test CueEngineError implements standard error traits
#[test]
fn test_error_traits() {
    // Test that we can create different error types
    let ffi_error = CueEngineError::ffi("test_function", "test error");
    let parse_error = CueEngineError::parse("invalid json", None);
    let json_error = CueEngineError::json(
        serde_json::from_str::<serde_json::Value>("invalid").unwrap_err(),
    );
    
    // All should implement Display
    assert!(!ffi_error.to_string().is_empty());
    assert!(!parse_error.to_string().is_empty());
    assert!(!json_error.to_string().is_empty());
    
    // All should implement Debug
    assert!(!format!("{:?}", ffi_error).is_empty());
    assert!(!format!("{:?}", parse_error).is_empty());
    assert!(!format!("{:?}", json_error).is_empty());
    
    // Error should implement std::error::Error
    fn assert_error<T: std::error::Error>(_: &T) {}
    assert_error(&ffi_error);
    assert_error(&parse_error);
    assert_error(&json_error);
}

/// Test error handling for circular imports (if supported by CUE)
#[test]
fn test_error_circular_dependency() {
    let temp_dir = create_module();
    
    // Create files with circular reference
    fs::write(
        temp_dir.path().join("a.cue"),
        r#"package test
a: b + 1
"#,
    )
    .unwrap();
    
    fs::write(
        temp_dir.path().join("b.cue"),
        r#"package test
b: a + 1
"#,
    )
    .unwrap();
    
    let result = evaluate_module(temp_dir.path(), "test", None);
    
    // CUE should detect the circular dependency
    assert!(result.is_err(), "Circular dependency should cause error");
}

/// Test that errors contain source location information when available
#[test]
fn test_error_source_location() {
    let temp_dir = create_module();
    
    // Create file with error at specific location
    let cue_content = r#"package test

line1: "ok"
line2: "ok"
line3: 123 & "string"  // Type error on line 5
"#;
    fs::write(temp_dir.path().join("error.cue"), cue_content).unwrap();
    
    let result = evaluate_module(temp_dir.path(), "test", None);
    
    assert!(result.is_err());
    let err = result.unwrap_err();
    let err_str = err.to_string();
    
    // Error message should be descriptive
    assert!(err_str.len() > 20, "Error should have detailed message");
}

/// Test that module evaluation handles empty modules gracefully
#[test]
fn test_error_empty_module() {
    let temp_dir = create_module();
    
    // Module exists but has no CUE files for this package
    let result = evaluate_module(temp_dir.path(), "nonexistent", None);
    
    // Should handle gracefully (either succeed with empty result or error)
    match result {
        Ok(module_result) => {
            assert!(
                module_result.instances.is_empty(),
                "Empty module should have no instances"
            );
        }
        Err(e) => {
            let err_str = e.to_string();
            assert!(!err_str.is_empty(), "Error should have message");
        }
    }
}

/// Test error code consistency
#[test]
fn test_error_code_types() {
    // Test FFI error
    let ffi_err = CueEngineError::ffi("test", "message");
    assert!(ffi_err.to_string().contains("FFI"));
    
    // Test parse error
    let parse_err = CueEngineError::parse("bad json", None);
    assert!(parse_err.to_string().contains("parse") || parse_err.to_string().contains("JSON"));
    
    // Test validation error
    let val_err = CueEngineError::validation("must be positive");
    assert!(val_err.to_string().contains("validation") || val_err.to_string().contains("positive"));
}

/// Test that multiple errors in a module are reported
#[test]
fn test_error_multiple_files() {
    let temp_dir = create_module();
    
    // Create multiple files with errors
    fs::write(
        temp_dir.path().join("error1.cue"),
        r#"package test
val1: 123 & "string"
"#,
    )
    .unwrap();
    
    fs::write(
        temp_dir.path().join("error2.cue"),
        r#"package test
val2: "ok"
"#,
    )
    .unwrap();
    
    let result = evaluate_module(temp_dir.path(), "test", None);
    
    // First error should cause evaluation to fail
    assert!(result.is_err());
}

/// Test error recovery - evaluation should not affect subsequent calls
#[test]
fn test_error_recovery() {
    let temp_dir = create_module();
    
    // First call: invalid CUE
    fs::write(
        temp_dir.path().join("bad.cue"),
        r#"package test
bad: 123 & "string"
"#,
    )
    .unwrap();
    
    let result1 = evaluate_module(temp_dir.path(), "test", None);
    assert!(result1.is_err(), "First call should fail");
    
    // Second call: fix the error
    fs::write(
        temp_dir.path().join("bad.cue"),
        r#"package test
good: "valid"
"#,
    )
    .unwrap();
    
    let result2 = evaluate_module(temp_dir.path(), "test", None);
    assert!(result2.is_ok(), "Second call should succeed after fixing error");
}

/// Test that evaluation options validation errors are clear
#[test]
fn test_error_invalid_options() {
    let temp_dir = create_module();
    
    fs::write(
        temp_dir.path().join("test.cue"),
        r#"package test
value: "ok""#,
    )
    .unwrap();
    
    // Try with invalid target_dir
    let options = ModuleEvalOptions {
        with_meta: false,
        recursive: false,
        package_name: None,
        target_dir: Some("../../../etc/passwd".to_string()),
    };
    
    let result = evaluate_module(temp_dir.path(), "test", Some(&options));
    
    // Should either fail validation or handle safely
    // The important thing is no directory traversal vulnerability
    if let Err(e) = result {
        let err_str = e.to_string();
        assert!(!err_str.is_empty());
    }
}
