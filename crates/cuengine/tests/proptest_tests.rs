//! Property-based tests for cuengine

use proptest::prelude::*;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn create_module(root: &Path) -> std::io::Result<()> {
    fs::create_dir_all(root.join("cue.mod"))?;
    fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"test.example/properties@v0\"\nlanguage: version: \"v0.14.1\"\n",
    )
}

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
        create_module(temp_dir.path()).unwrap();
        fs::write(temp_dir.path().join("test.cue"), &content).unwrap();
        let json = cuengine::evaluate_cue_package(temp_dir.path(), "testpkg").unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let fields = value["env"].as_object().unwrap();
        prop_assert_eq!(fields.len(), num_fields);
        for index in 0..num_fields {
            prop_assert_eq!(&fields[&format!("field_{index}")], &format!("value_{index}"));
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
        create_module(&path).unwrap();

        // Create a simple CUE file
        fs::write(path.join("test.cue"), "package test\nenv: {TEST: \"value\"}").unwrap();

        let json = cuengine::evaluate_cue_package(&path, "test").unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&value["env"]["TEST"], "value");
    }

    #[test]
    fn test_package_name_fuzzing(
        suffix in "[a-zA-Z0-9_]{0,50}"
    ) {
        // The fixed prefix excludes reserved CUE keywords while exercising identifiers.
        let name = format!("pkg_{suffix}");
        let temp_dir = TempDir::new().unwrap();
        create_module(temp_dir.path()).unwrap();
        let content = format!("package {name}\nenv: {{TEST: \"value\"}}");
        fs::write(temp_dir.path().join("test.cue"), content).unwrap();

        let json = cuengine::evaluate_cue_package(temp_dir.path(), &name).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&value["env"]["TEST"], "value");
    }
}
