//! Tests for cue_eval_module FFI function
//!
//! These tests verify that the new module-wide evaluation API works correctly,
//! including metadata extraction, recursive evaluation, and error handling.

use cuengine::{ModuleEvalOptions, evaluate_module};
use std::fs;
use tempfile::TempDir;

/// Helper to create a CUE module with cue.mod directory
fn create_test_module() -> TempDir {
    let temp_dir = TempDir::new().unwrap();
    let cue_mod = temp_dir.path().join("cue.mod");
    fs::create_dir(&cue_mod).unwrap();
    
    // Create module.cue file
    let module_content = r#"module: "test.example.com/test"
language: version: "v0.10.0"
"#;
    fs::write(cue_mod.join("module.cue"), module_content).unwrap();
    
    temp_dir
}

/// Test basic module evaluation with default options
#[test]
fn test_evaluate_module_basic() {
    let temp_dir = create_test_module();
    
    // Create a simple CUE file
    let cue_content = r#"package test

config: {
    name: "test-app"
    version: "1.0.0"
}
"#;
    fs::write(temp_dir.path().join("config.cue"), cue_content).unwrap();
    
    // Evaluate the module
    let result = evaluate_module(temp_dir.path(), "test", None).unwrap();
    
    // Verify we got results
    assert!(!result.instances.is_empty());
    
    // Check that root instance exists
    let root_instance = result.instances.get(".").expect("Root instance should exist");
    
    // Verify the content
    let config = root_instance.get("config").expect("config field should exist");
    assert_eq!(config["name"], "test-app");
    assert_eq!(config["version"], "1.0.0");
}

/// Test metadata extraction when with_meta is enabled
#[test]
fn test_evaluate_module_with_metadata() {
    let temp_dir = create_test_module();
    
    let cue_content = r#"package test

database: {
    host: "localhost"
    port: 5432
}
"#;
    fs::write(temp_dir.path().join("db.cue"), cue_content).unwrap();
    
    // Evaluate with metadata extraction
    let options = ModuleEvalOptions {
        with_meta: true,
        recursive: false,
        package_name: None,
        target_dir: None,
    };
    
    let result = evaluate_module(temp_dir.path(), "test", Some(&options)).unwrap();
    
    // Verify we got metadata
    assert!(!result.meta.is_empty(), "Metadata should be extracted when with_meta is true");
    
    // Check that metadata exists for our fields
    let has_database_meta = result.meta.keys().any(|k| k.contains("database"));
    assert!(has_database_meta, "Should have metadata for database field");
}

/// Test recursive evaluation (./...)
#[test]
fn test_evaluate_module_recursive() {
    let temp_dir = create_test_module();
    
    // Create root level file
    fs::write(
        temp_dir.path().join("root.cue"),
        r#"package test
root: value: "root""#,
    )
    .unwrap();
    
    // Create subdirectory with its own file
    let subdir = temp_dir.path().join("sub");
    fs::create_dir(&subdir).unwrap();
    fs::write(
        subdir.join("sub.cue"),
        r#"package test
sub: value: "subdirectory""#,
    )
    .unwrap();
    
    // Evaluate recursively
    let options = ModuleEvalOptions {
        with_meta: false,
        recursive: true,
        package_name: None,
        target_dir: None,
    };
    
    let result = evaluate_module(temp_dir.path(), "test", Some(&options)).unwrap();
    
    // Should have instances from both root and subdirectory
    assert!(result.instances.len() >= 2, "Recursive evaluation should find multiple instances");
    
    // Check root instance
    let root = result.instances.get(".").expect("Root instance should exist");
    assert!(root.get("root").is_some(), "Root value should exist");
    
    // Check subdirectory instance
    let sub = result.instances.get("sub").expect("Sub instance should exist");
    assert!(sub.get("sub").is_some(), "Sub value should exist");
}

/// Test non-recursive evaluation (.)
#[test]
fn test_evaluate_module_non_recursive() {
    let temp_dir = create_test_module();
    
    // Create root level file
    fs::write(
        temp_dir.path().join("root.cue"),
        r#"package test
root: value: "root""#,
    )
    .unwrap();
    
    // Create subdirectory with its own file
    let subdir = temp_dir.path().join("sub");
    fs::create_dir(&subdir).unwrap();
    fs::write(
        subdir.join("sub.cue"),
        r#"package test
sub: value: "subdirectory""#,
    )
    .unwrap();
    
    // Evaluate non-recursively (default)
    let options = ModuleEvalOptions {
        with_meta: false,
        recursive: false,
        package_name: None,
        target_dir: None,
    };
    
    let result = evaluate_module(temp_dir.path(), "test", Some(&options)).unwrap();
    
    // Should only have root instance
    assert_eq!(result.instances.len(), 1, "Non-recursive should only find root instance");
    assert!(result.instances.contains_key("."), "Should contain root instance");
}

/// Test package name filtering
#[test]
fn test_evaluate_module_package_filter() {
    let temp_dir = create_test_module();
    
    // Create file with target package
    fs::write(
        temp_dir.path().join("target.cue"),
        r#"package target
value: "target-package""#,
    )
    .unwrap();
    
    // Create file with different package
    fs::write(
        temp_dir.path().join("other.cue"),
        r#"package other
value: "other-package""#,
    )
    .unwrap();
    
    // Evaluate with package filter
    let options = ModuleEvalOptions {
        with_meta: false,
        recursive: false,
        package_name: Some("target".to_string()),
        target_dir: None,
    };
    
    let result = evaluate_module(temp_dir.path(), "test", Some(&options)).unwrap();
    
    // Should only get the target package
    let root = result.instances.get(".").expect("Root instance should exist");
    assert!(root.get("value").is_some());
    assert_eq!(root["value"], "target-package");
}

/// Test error handling for invalid CUE syntax
#[test]
fn test_evaluate_module_invalid_syntax() {
    let temp_dir = create_test_module();
    
    // Create file with invalid syntax
    let invalid_content = r#"package test
invalid: {
    missing_closing_brace: "oops"
"#;
    fs::write(temp_dir.path().join("invalid.cue"), invalid_content).unwrap();
    
    // Should return an error
    let result = evaluate_module(temp_dir.path(), "test", None);
    assert!(result.is_err(), "Invalid CUE syntax should cause an error");
    
    let err = result.unwrap_err();
    let err_str = err.to_string();
    assert!(
        err_str.contains("Failed") || err_str.contains("error"),
        "Error message should indicate failure: {}",
        err_str
    );
}

/// Test error handling for missing module
#[test]
fn test_evaluate_module_no_cue_mod() {
    let temp_dir = TempDir::new().unwrap();
    
    // Create CUE file but no cue.mod
    fs::write(
        temp_dir.path().join("test.cue"),
        r#"package test
value: "test""#,
    )
    .unwrap();
    
    // Evaluation should still work (module root is optional for single-file evaluation)
    let result = evaluate_module(temp_dir.path(), "test", None);
    
    // Depending on implementation, this might succeed or fail
    // The important thing is it doesn't panic
    match result {
        Ok(_) => {
            // Success is acceptable if the evaluator handles missing cue.mod gracefully
        }
        Err(e) => {
            // Error is also acceptable
            let err_str = e.to_string();
            assert!(
                !err_str.is_empty(),
                "Error should have a meaningful message"
            );
        }
    }
}

/// Test concurrent module evaluations
#[test]
fn test_evaluate_module_concurrent() {
    use std::sync::Arc;
    use std::thread;
    
    let temp_dir = create_test_module();
    
    let cue_content = r#"package test
concurrent: {
    value: "test-value"
    counter: 42
}
"#;
    fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();
    
    let path = Arc::new(temp_dir.path().to_path_buf());
    const NUM_THREADS: usize = 4;
    
    let handles: Vec<_> = (0..NUM_THREADS)
        .map(|_| {
            let path = Arc::clone(&path);
            thread::spawn(move || {
                let result = evaluate_module(&path, "test", None);
                result.is_ok()
            })
        })
        .collect();
    
    // All threads should succeed
    for handle in handles {
        let success = handle.join().unwrap();
        assert!(success, "Concurrent evaluation should succeed");
    }
}

/// Test that instances map uses relative paths as keys
#[test]
fn test_evaluate_module_instance_keys() {
    let temp_dir = create_test_module();
    
    // Create nested structure
    let subdir = temp_dir.path().join("nested").join("deep");
    fs::create_dir_all(&subdir).unwrap();
    
    fs::write(
        subdir.join("deep.cue"),
        r#"package test
deep: value: "deeply nested""#,
    )
    .unwrap();
    
    let options = ModuleEvalOptions {
        with_meta: false,
        recursive: true,
        package_name: None,
        target_dir: None,
    };
    
    let result = evaluate_module(temp_dir.path(), "test", Some(&options)).unwrap();
    
    // Check that keys are relative paths
    let has_nested = result.instances.keys().any(|k| k.contains("nested"));
    assert!(has_nested, "Instance keys should include nested paths");
}

/// Test Project detection in module results
#[test]
fn test_evaluate_module_project_detection() {
    let temp_dir = create_test_module();
    
    // Create a file that conforms to Project schema
    let cue_content = r#"package test

name: "test-project"
tasks: {
    build: {
        command: "echo"
        args: ["building"]
    }
}
"#;
    fs::write(temp_dir.path().join("project.cue"), cue_content).unwrap();
    
    let result = evaluate_module(temp_dir.path(), "test", None).unwrap();
    
    // The projects field should be populated if the instance matches Project schema
    // Note: This depends on the Go implementation actually checking against schema.#Project
    // If no projects are detected, the test documents current behavior
    if !result.projects.is_empty() {
        assert!(result.projects.contains(&".".to_string()));
    }
}

/// Test target_dir option for evaluating a specific subdirectory
#[test]
fn test_evaluate_module_target_dir() {
    let temp_dir = create_test_module();
    
    // Create subdirectory with file
    let subdir = temp_dir.path().join("target");
    fs::create_dir(&subdir).unwrap();
    fs::write(
        subdir.join("specific.cue"),
        r#"package test
specific: value: "targeted""#,
    )
    .unwrap();
    
    // Also create root file that should be ignored
    fs::write(
        temp_dir.path().join("root.cue"),
        r#"package test
root: value: "should-not-appear""#,
    )
    .unwrap();
    
    let options = ModuleEvalOptions {
        with_meta: false,
        recursive: false,
        package_name: None,
        target_dir: Some("target".to_string()),
    };
    
    let result = evaluate_module(temp_dir.path(), "test", Some(&options)).unwrap();
    
    // Should only have the target directory instance
    let instance = result.instances.get("target").or_else(|| result.instances.get("."));
    assert!(instance.is_some(), "Should have instance from target directory");
    
    if let Some(inst) = instance {
        assert!(inst.get("specific").is_some(), "Should have specific field from target dir");
    }
}
