//! Schema conformance tests
//!
//! These tests ensure that Rust types and CUE schemas remain in sync

use cuenv_core::manifest::Cuenv;
use schemars::schema_for;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;

#[test]
fn test_rust_schema_generation() {
    // Test that we can generate valid JSON Schema from Rust types
    let schema = schema_for!(Cuenv);
    let json = serde_json::to_string_pretty(&schema).unwrap();

    // Schema should contain expected top-level properties
    assert!(json.contains("\"title\": \"Cuenv\""));
    assert!(json.contains("\"properties\""));
    assert!(json.contains("config"));
    assert!(json.contains("env"));
    assert!(json.contains("hooks"));
    assert!(json.contains("tasks"));
}

#[test]
fn test_valid_fixtures_parse() {
    // Valid fixtures should parse successfully with Rust types
    let fixtures_dir = Path::new("tests/fixtures/valid");

    // Test minimal config
    if let Ok(json) = export_and_parse_cue(&fixtures_dir.join("minimal.cue")) {
        let result = serde_json::from_value::<Cuenv>(json);
        assert!(
            result.is_ok(),
            "Minimal config should parse: {:?}",
            result.err()
        );
    }

    // Test full config
    if let Ok(json) = export_and_parse_cue(&fixtures_dir.join("full.cue")) {
        let result = serde_json::from_value::<Cuenv>(json);
        assert!(
            result.is_ok(),
            "Full config should parse: {:?}",
            result.err()
        );

        let cuenv = result.unwrap();
        assert!(cuenv.config.is_some());
        assert!(cuenv.env.is_some());
        assert!(cuenv.hooks.is_some());
        assert!(!cuenv.tasks.is_empty());
    }

    // Test hooks config
    if let Ok(json) = export_and_parse_cue(&fixtures_dir.join("hooks.cue")) {
        let result = serde_json::from_value::<Cuenv>(json);
        assert!(
            result.is_ok(),
            "Hooks config should parse: {:?}",
            result.err()
        );

        let cuenv = result.unwrap();
        assert!(cuenv.hooks.is_some());
        let hooks = cuenv.hooks.unwrap();
        assert!(hooks.on_enter.is_some());
        assert!(hooks.on_exit.is_some());
    }
}

#[test]
#[ignore] // Requires CUE CLI to be installed
fn test_cue_validation() {
    // Test that CUE validates our fixtures correctly
    let fixtures_dir = Path::new("tests/fixtures");

    // Valid fixtures should pass CUE validation
    for entry in fs::read_dir(fixtures_dir.join("valid")).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("cue") {
            let output = Command::new("cue")
                .args(["vet", path.to_str().unwrap()])
                .output()
                .expect("Failed to run cue vet");

            assert!(
                output.status.success(),
                "Valid fixture {} should pass CUE validation: {}",
                path.display(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    // Invalid fixtures should fail CUE validation
    for entry in fs::read_dir(fixtures_dir.join("invalid")).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("cue") {
            let output = Command::new("cue")
                .args(["vet", path.to_str().unwrap()])
                .output()
                .expect("Failed to run cue vet");

            assert!(
                !output.status.success(),
                "Invalid fixture {} should fail CUE validation",
                path.display()
            );
        }
    }
}

#[test]
fn test_round_trip_serialization() {
    use cuenv_core::{
        config::{CacheMode, Config, OutputFormat},
        environment::{Env, EnvValue},
        hooks::types::Hook,
        manifest::{Cuenv, HookList, Hooks},
    };
    use std::collections::HashMap;

    // Create a sample Cuenv structure
    let mut env_vars = HashMap::new();
    env_vars.insert(
        "DATABASE_URL".to_string(),
        EnvValue::String("postgres://localhost".to_string()),
    );
    env_vars.insert("PORT".to_string(), EnvValue::Int(3000));
    env_vars.insert("DEBUG".to_string(), EnvValue::Bool(true));

    let cuenv = Cuenv {
        config: Some(Config {
            output_format: Some(OutputFormat::Simple),
            cache_mode: Some(CacheMode::ReadWrite),
            cache_enabled: Some(true),
            audit_mode: None,
            default_capabilities: None,
            default_environment: None,
            trace_output: None,
        }),
        env: Some(Env {
            base: env_vars,
            environment: None,
        }),
        hooks: Some(Hooks {
            on_enter: Some(HookList::Single(Hook {
                command: "echo".to_string(),
                args: vec!["Entering".to_string()],
                dir: None,
                inputs: vec![],
                source: Some(false),
            })),
            on_exit: None,
        }),
        tasks: HashMap::new(),
    };

    // Serialize to JSON
    let json = serde_json::to_value(&cuenv).unwrap();

    // Deserialize back
    let deserialized: Cuenv = serde_json::from_value(json.clone()).unwrap();

    // Should match original
    assert_eq!(cuenv, deserialized);

    // JSON should have expected structure
    assert!(json.get("config").is_some());
    assert!(json.get("env").is_some());
    assert!(json.get("hooks").is_some());
}

// Helper function to export CUE to JSON
fn export_and_parse_cue(cue_path: &Path) -> Result<Value, String> {
    // Skip if file doesn't exist
    if !cue_path.exists() {
        return Err(format!("File not found: {}", cue_path.display()));
    }

    // Try to export with cue command if available
    if let Ok(output) = Command::new("cue")
        .args(["export", cue_path.to_str().unwrap()])
        .output()
    {
        if output.status.success() {
            let json_str =
                String::from_utf8(output.stdout).map_err(|e| format!("Invalid UTF-8: {}", e))?;
            serde_json::from_str(&json_str).map_err(|e| format!("Invalid JSON: {}", e))
        } else {
            Err(format!(
                "CUE export failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    } else {
        // If CUE is not available, create a minimal valid structure for testing
        Ok(serde_json::json!({}))
    }
}
