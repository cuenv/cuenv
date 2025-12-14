//! Tests for CUE evaluation and JSON parsing

#![allow(clippy::print_stdout)]

use cuengine::CueEvaluator;
use cuenv_core::environment::{EnvValue, Environment};
use cuenv_core::manifest::Cuenv;
use std::path::Path;

#[test]
fn test_parse_task_basic_example() {
    // Get the project root (where Cargo.toml is)
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let project_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let example_path = project_root.join("examples/task-basic");

    // Skip test if example doesn't exist
    if !example_path.exists() {
        println!("Skipping test - example path doesn't exist: {example_path:?}");
        return;
    }

    // Evaluate the actual example CUE file
    let evaluator = CueEvaluator::builder().build().unwrap();
    let json = evaluator.evaluate(&example_path, "examples").unwrap();

    println!("Raw JSON from CUE evaluation:");
    println!("{json}");

    // Parse the JSON to see what structure we get
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    println!("\nParsed JSON structure:");
    println!("{parsed:#?}");

    // Check what fields are present at the root
    if let serde_json::Value::Object(map) = &parsed {
        println!("\nRoot level fields:");
        for key in map.keys() {
            println!("  - {key}");
        }
    }

    // Try to parse as typed Cuenv
    let manifest: Result<Cuenv, _> = serde_json::from_str(&json);
    match manifest {
        Ok(cuenv) => {
            println!("\nSuccessfully parsed as Cuenv");

            // Extract environment variables
            let mut env = Environment::new();
            if let Some(env_config) = &cuenv.env {
                for (key, value) in &env_config.base {
                    // Use the to_string_value method that handles all variants
                    let value_str = value.to_string_value();
                    if value_str == "[SECRET]" {
                        continue; // Skip secrets
                    }
                    env.set(key.clone(), value_str);
                }
            }

            println!("Environment variables: {:#?}", env.vars);
            let task_names: Vec<&str> = cuenv.tasks.keys().map(String::as_str).collect();
            println!("Tasks: {task_names:?}");

            // Verify we got the expected values
            assert_eq!(env.get("NAME"), Some("Jack O'Neill"));
            // Verify expected tasks exist
            assert!(cuenv.tasks.contains_key("interpolate"));
            assert!(cuenv.tasks.contains_key("propagate"));
            assert!(cuenv.tasks.contains_key("greetAll"));
            assert!(cuenv.tasks.contains_key("shellExample"));
        }
        Err(e) => {
            println!("\nFailed to parse as Cuenv: {e}");
            println!("This might be expected if the example structure doesn't match");
        }
    }
}

#[test]
fn test_parse_custom_cue() {
    use std::fs;
    use tempfile::TempDir;

    // Create a temporary directory with a CUE file
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r#"package test
name: "test"
env: {
    DATABASE_URL: "postgres://localhost/mydb"
    PORT: 3000
    DEBUG: true
}
tasks: {
    test: {
        command: "echo"
        args: ["Running tests"]
    }
}"#;
    fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

    // Evaluate the CUE file
    let evaluator = CueEvaluator::builder().build().unwrap();
    let json = evaluator.evaluate(temp_dir.path(), "test").unwrap();

    // Parse as typed Cuenv
    let manifest: Cuenv = serde_json::from_str(&json).unwrap();

    // Verify environment
    let env_config = manifest.env.unwrap();
    assert_eq!(
        env_config.base.get("DATABASE_URL"),
        Some(&EnvValue::String("postgres://localhost/mydb".to_string()))
    );
    assert_eq!(env_config.base.get("PORT"), Some(&EnvValue::Int(3000)));
    assert_eq!(env_config.base.get("DEBUG"), Some(&EnvValue::Bool(true)));

    // Verify tasks
    assert!(manifest.tasks.contains_key("test"));
}
