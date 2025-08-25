//! Property-based tests for cuengine

use proptest::prelude::*;
use cuengine::*;
use tempfile::TempDir;
use std::fs;

// Generate random valid CUE content
fn generate_cue_content(num_fields: usize) -> String {
    let field_lines: Vec<String> = (0..num_fields)
        .map(|i| format!("    field_{i}: \"value_{i}\""))
        .collect();
    
    format!("package testpkg\n\nenv: {{\n{}\n}}", field_lines.join("\n"))
}

proptest! {
    #[test]
    fn test_random_valid_cue(num_fields in 1..20usize) {
        let content = generate_cue_content(num_fields);
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("test.cue"), &content).unwrap();
        
        // Should not panic with valid CUE
        let result = evaluate_cue_package(temp_dir.path(), "testpkg");
        
        match result {
            Ok(json) => {
                // Valid CUE should produce valid JSON
                let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
                assert!(parsed.is_object());
            }
            Err(e) => {
                // If it fails, error should be meaningful
                let error_str = e.to_string();
                assert!(!error_str.is_empty());
                assert!(error_str.len() > 10);
            }
        }
    }
    
    #[test]
    fn test_path_handling(
        segments in prop::collection::vec("[a-z0-9_-]{1,20}", 1..5)
    ) {
        let temp_dir = TempDir::new().unwrap();
        
        // Build a nested path
        let mut path = temp_dir.path().to_path_buf();
        for segment in &segments {
            path = path.join(segment);
        }
        fs::create_dir_all(&path).unwrap();
        
        // Create a simple CUE file
        fs::write(path.join("test.cue"), "package test\nenv: {TEST: \"value\"}").unwrap();
        
        // Should handle nested paths correctly
        let result = evaluate_cue_package(&path, "test");
        
        // Should either succeed or fail gracefully
        if let Err(e) = result {
            let error_str = e.to_string();
            assert!(!error_str.is_empty());
        }
    }
    
    #[test]
    fn test_package_name_fuzzing(
        name in "[a-zA-Z][a-zA-Z0-9_]{0,50}"
    ) {
        let temp_dir = TempDir::new().unwrap();
        let content = format!("package {name}\nenv: {{TEST: \"value\"}}");
        fs::write(temp_dir.path().join("test.cue"), content).unwrap();
        
        // Should handle various package names
        let result = evaluate_cue_package(temp_dir.path(), &name);
        
        // Should not panic
        match result {
            Ok(_) => { /* success */ }
            Err(e) => {
                assert!(!e.to_string().is_empty());
            }
        }
    }
}