//! Tests for CUE evaluation and JSON parsing

use cuengine::CueEvaluator;
use cuenv_core::environment::{CueEvaluation, Environment};
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

    // Try to parse as CueEvaluation
    let evaluation = CueEvaluation::from_json(&json);
    match evaluation {
        Ok(eval) => {
            println!("\nSuccessfully parsed as CueEvaluation");
            let env = eval.get_environment();
            println!("Environment variables: {:?}", env.vars);
            println!("Tasks: {:?}", eval.tasks.list_tasks());

            // Verify we got the expected values
            assert_eq!(env.get("NAME"), Some("Jack O'Neill"));
            assert!(eval.tasks.contains("interpolate"));
            assert!(eval.tasks.contains("propagate"));
            assert!(eval.tasks.contains("greetAll"));
            assert!(eval.tasks.contains("greetIndividual"));
        }
        Err(e) => {
            println!("\nFailed to parse as CueEvaluation: {e}");
            panic!("Failed to parse CUE evaluation");
        }
    }
}

#[test]
fn test_parse_env_basic_example() {
    // Get the project root
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let project_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let example_path = project_root.join("examples/env-basic");

    // Skip test if example doesn't exist
    if !example_path.exists() {
        println!("Skipping test - example path doesn't exist: {example_path:?}");
        return;
    }

    // Test with env-basic example
    let evaluator = CueEvaluator::builder().build().unwrap();
    let json = evaluator.evaluate(&example_path, "examples").unwrap();

    println!("Raw JSON from env-basic:");
    println!("{json}");

    // Parse to inspect structure
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    // Check for env field
    if let Some(env) = parsed.get("env") {
        println!("\nFound 'env' field:");
        println!("{env:#?}");
    } else {
        println!("\nNo 'env' field found at root level");
    }
}

#[test]
fn test_environment_operations() {
    let mut env = Environment::new();

    assert!(env.is_empty());
    assert_eq!(env.len(), 0);

    env.set("KEY1".to_string(), "value1".to_string());
    env.set("KEY2".to_string(), "value2".to_string());

    assert!(!env.is_empty());
    assert_eq!(env.len(), 2);

    assert_eq!(env.get("KEY1"), Some("value1"));
    assert_eq!(env.get("KEY2"), Some("value2"));
    assert_eq!(env.get("MISSING"), None);

    assert!(env.contains("KEY1"));
    assert!(!env.contains("MISSING"));
}

#[test]
fn test_environment_from_map() {
    let mut map = std::collections::HashMap::new();
    map.insert("TEST_VAR".to_string(), "test_value".to_string());
    map.insert("ANOTHER_VAR".to_string(), "another_value".to_string());

    let env = Environment::from_map(map);

    assert_eq!(env.len(), 2);
    assert_eq!(env.get("TEST_VAR"), Some("test_value"));
    assert_eq!(env.get("ANOTHER_VAR"), Some("another_value"));
}

#[test]
fn test_environment_to_env_vec() {
    let mut env = Environment::new();
    env.set("VAR1".to_string(), "value1".to_string());
    env.set("VAR2".to_string(), "value2".to_string());

    let env_vec = env.to_env_vec();

    assert_eq!(env_vec.len(), 2);
    assert!(env_vec.contains(&"VAR1=value1".to_string()));
    assert!(env_vec.contains(&"VAR2=value2".to_string()));
}

#[test]
#[allow(unsafe_code)] // Required for test environment variable manipulation
fn test_environment_merge_with_system() {
    let mut env = Environment::new();
    env.set("CUENV_TEST_VAR".to_string(), "override_value".to_string());
    env.set("CUENV_NEW_VAR".to_string(), "new_value".to_string());

    // Set a system environment variable temporarily
    unsafe {
        std::env::set_var("CUENV_TEST_VAR", "system_value");
        std::env::set_var("CUENV_SYSTEM_VAR", "system_only");
    }

    let merged = env.merge_with_system();

    // CUE env should override system env
    assert_eq!(
        merged.get("CUENV_TEST_VAR"),
        Some(&"override_value".to_string())
    );
    assert_eq!(merged.get("CUENV_NEW_VAR"), Some(&"new_value".to_string()));
    assert_eq!(
        merged.get("CUENV_SYSTEM_VAR"),
        Some(&"system_only".to_string())
    );

    // Clean up
    unsafe {
        std::env::remove_var("CUENV_TEST_VAR");
        std::env::remove_var("CUENV_SYSTEM_VAR");
    }
}

#[test]
fn test_environment_iterator() {
    let mut env = Environment::new();
    env.set("A".to_string(), "1".to_string());
    env.set("B".to_string(), "2".to_string());

    let mut collected: Vec<_> = env.iter().collect();
    collected.sort_by_key(|(k, _)| k.as_str());

    assert_eq!(collected.len(), 2);
    assert_eq!(collected[0], (&"A".to_string(), &"1".to_string()));
    assert_eq!(collected[1], (&"B".to_string(), &"2".to_string()));
}

#[test]
fn test_cue_evaluation_from_json() {
    let json = r#"{
        "env": {
            "NAME": "TestName",
            "VERSION": "1.0.0",
            "environment": "should_be_skipped"
        },
        "tasks": {
            "test_task": {
                "command": "echo",
                "args": ["test"]
            }
        }
    }"#;

    let evaluation = CueEvaluation::from_json(json).unwrap();
    let env = evaluation.get_environment();

    assert_eq!(env.get("NAME"), Some("TestName"));
    assert_eq!(env.get("VERSION"), Some("1.0.0"));
    assert_eq!(env.get("environment"), None); // Should be skipped

    assert!(evaluation.tasks.contains("test_task"));
}

#[test]
fn test_environment_extraction_with_various_types() {
    let json = r#"{
        "env": {
            "STRING_VAR": "string_value",
            "NUMBER_VAR": 42,
            "BOOL_VAR": true,
            "NULL_VAR": null,
            "ARRAY_VAR": ["item1", "item2"],
            "OBJECT_VAR": {"nested": "value"}
        }
    }"#;

    let evaluation = CueEvaluation::from_json(json).unwrap();
    let env = evaluation.get_environment();

    assert_eq!(env.get("STRING_VAR"), Some("string_value"));
    assert_eq!(env.get("NUMBER_VAR"), Some("42"));
    assert_eq!(env.get("BOOL_VAR"), Some("true"));
    assert_eq!(env.get("NULL_VAR"), None); // null values are skipped
    assert_eq!(env.get("ARRAY_VAR"), Some("[\"item1\",\"item2\"]"));
    assert_eq!(env.get("OBJECT_VAR"), Some("{\"nested\":\"value\"}"));
}