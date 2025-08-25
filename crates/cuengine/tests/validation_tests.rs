//! Tests for input validation functions

use cuengine::validation::{validate_path, validate_package_name, validate_output};
use cuenv_core::Limits;
use tempfile::TempDir;
use std::fs;
use std::path::Path;

#[test]
fn test_validate_path_exists() {
    let limits = Limits::default();
    let temp_dir = TempDir::new().unwrap();
    
    // Valid directory should pass
    let result = validate_path(temp_dir.path(), &limits);
    assert!(result.is_ok());
    
    // Non-existent path should fail
    let non_existent = temp_dir.path().join("non_existent");
    let result = validate_path(&non_existent, &limits);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("does not exist"));
}

#[test]
fn test_validate_path_is_directory() {
    let limits = Limits::default();
    let temp_dir = TempDir::new().unwrap();
    
    // Create a file
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "test").unwrap();
    
    // File should fail validation
    let result = validate_path(&file_path, &limits);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not a directory"));
}

#[test]
fn test_validate_path_length() {
    let limits = Limits {
        max_path_length: 10,
        ..Default::default()
    };
    
    let temp_dir = TempDir::new().unwrap();
    
    // Short path should pass
    let short_dir = temp_dir.path().join("a");
    fs::create_dir(&short_dir).unwrap();
    let result = validate_path(&short_dir, &limits);
    
    // This might still fail if temp path is long, so just check error type
    if result.is_err() {
        assert!(result.unwrap_err().to_string().contains("exceeds maximum length"));
    }
    
    // Long path should fail
    let limits_strict = Limits {
        max_path_length: 1,
        ..Default::default()
    };
    let result = validate_path(temp_dir.path(), &limits_strict);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("exceeds maximum length"));
}

#[test]
fn test_validate_path_traversal() {
    let limits = Limits::default();
    
    // Path with parent directory traversal should fail
    let path_with_traversal = Path::new("/tmp/../etc");
    let result = validate_path(path_with_traversal, &limits);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("parent directory traversal"));
}

#[test]
fn test_validate_package_name_empty() {
    let limits = Limits::default();
    
    let result = validate_package_name("", &limits);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("cannot be empty"));
}

#[test]
fn test_validate_package_name_length() {
    let limits = Limits {
        max_package_name_length: 10,
        ..Default::default()
    };
    
    // Valid length
    let result = validate_package_name("short", &limits);
    assert!(result.is_ok());
    
    // Too long
    let result = validate_package_name("this_is_a_very_long_package_name", &limits);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("exceeds maximum length"));
}

#[test]
fn test_validate_package_name_characters() {
    let limits = Limits::default();
    
    // Valid names
    assert!(validate_package_name("valid_name", &limits).is_ok());
    assert!(validate_package_name("valid-name", &limits).is_ok());
    assert!(validate_package_name("validName123", &limits).is_ok());
    
    // Invalid characters
    let result = validate_package_name("invalid.name", &limits);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("invalid characters"));
    
    let result = validate_package_name("invalid name", &limits);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("invalid characters"));
    
    let result = validate_package_name("invalid@name", &limits);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("invalid characters"));
}

#[test]
fn test_validate_package_name_first_character() {
    let limits = Limits::default();
    
    // Must start with alphabetic
    let result = validate_package_name("1invalid", &limits);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("must start with an alphabetic"));
    
    let result = validate_package_name("-invalid", &limits);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("must start with an alphabetic"));
    
    let result = validate_package_name("_invalid", &limits);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("must start with an alphabetic"));
    
    // Valid first character
    assert!(validate_package_name("valid", &limits).is_ok());
    assert!(validate_package_name("Valid", &limits).is_ok());
}

#[test]
fn test_validate_output_size() {
    let limits = Limits {
        max_output_size: 100,
        ..Default::default()
    };
    
    // Small output should pass
    let small_output = "small output";
    assert!(validate_output(small_output, &limits).is_ok());
    
    // Large output should fail
    let large_output = "x".repeat(101);
    let result = validate_output(&large_output, &limits);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("exceeds maximum size"));
    
    // Exactly at limit should pass
    let exact_output = "x".repeat(100);
    assert!(validate_output(&exact_output, &limits).is_ok());
}

#[test]
fn test_validate_package_name_edge_cases() {
    let limits = Limits::default();
    
    // Single character valid names
    assert!(validate_package_name("a", &limits).is_ok());
    assert!(validate_package_name("A", &limits).is_ok());
    
    // Names with numbers after first character
    assert!(validate_package_name("a1", &limits).is_ok());
    assert!(validate_package_name("test123", &limits).is_ok());
    
    // Names with underscores and hyphens
    assert!(validate_package_name("test_package", &limits).is_ok());
    assert!(validate_package_name("test-package", &limits).is_ok());
    assert!(validate_package_name("test_package-name", &limits).is_ok());
}

#[test]
fn test_validate_output_empty() {
    let limits = Limits::default();
    
    // Empty output should pass
    assert!(validate_output("", &limits).is_ok());
}