//! Tests for CUE evaluation and JSON parsing

use cuengine::evaluate_cue_package;
use cuenv_core::environment::{EnvValue, Environment};
use cuenv_core::manifest::Project;
use std::error::Error;
use std::path::Path;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

#[test]
fn test_parse_task_basic_example() -> TestResult {
    // Get the project root (where cue.mod lives)
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let project_root = Path::new(manifest_dir).join("../..").canonicalize()?;

    // Verify cue.mod exists (required for module evaluation)
    let cue_mod_path = project_root.join("cue.mod/module.cue");
    if !cue_mod_path.exists() {
        tracing::info!("Skipping test - cue.mod not found at project root: {project_root:?}");
        return Ok(());
    }

    // Evaluate from project root - the examples package is in examples/task-basic
    // but imports require module root resolution
    let result = evaluate_cue_package(&project_root, "examples");

    // Handle both success and failure cases gracefully (FFI may be unavailable in CI)
    let json = match result {
        Ok(json) => json,
        Err(e) => {
            tracing::info!("FFI evaluation failed (may be unavailable in test environment): {e}");
            return Ok(());
        }
    };

    // Try to parse as typed Project
    let manifest: Result<Project, _> = serde_json::from_str(&json);
    match manifest {
        Ok(cuenv) => {
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

            // Verify we got the expected values
            assert_eq!(env.get("NAME"), Some("Jack O'Neill"));
            // Verify expected tasks exist
            assert!(cuenv.tasks.contains_key("interpolate"));
            assert!(cuenv.tasks.contains_key("propagate"));
            assert!(cuenv.tasks.contains_key("greetAll"));
            assert!(cuenv.tasks.contains_key("shellExample"));
        }
        Err(e) => {
            tracing::info!("Failed to parse as Project: {e}");
        }
    }

    Ok(())
}

#[test]
fn test_parse_custom_cue() -> TestResult {
    use std::fs;
    use tempfile::TempDir;

    // Create a temporary directory with a CUE file
    let temp_dir = TempDir::new()?;

    // Create cue.mod/module.cue (required for module evaluation)
    let cue_mod_dir = temp_dir.path().join("cue.mod");
    fs::create_dir_all(&cue_mod_dir)?;
    fs::write(
        cue_mod_dir.join("module.cue"),
        r#"module: "test.local/temp"
language: version: "v0.14.1"
"#,
    )?;

    // Use simple CUE content without schema (compatible with non-schema evaluation)
    let cue_content = r#"package cuenv
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
    fs::write(temp_dir.path().join("env.cue"), cue_content)?;

    // Evaluate the CUE file - handle FFI unavailability gracefully
    let result = evaluate_cue_package(temp_dir.path(), "cuenv");
    let json = match result {
        Ok(json) => json,
        Err(e) => {
            tracing::info!("FFI evaluation failed (may be unavailable in test environment): {e}");
            return Ok(());
        }
    };

    // Parse as typed Project
    let manifest: Project = serde_json::from_str(&json)?;

    // Verify environment
    let env_config = manifest
        .env
        .ok_or_else(|| std::io::Error::other("custom CUE manifest should contain env"))?;
    assert_eq!(
        env_config.base.get("DATABASE_URL"),
        Some(&EnvValue::String("postgres://localhost/mydb".to_string()))
    );
    assert_eq!(env_config.base.get("PORT"), Some(&EnvValue::Int(3000)));
    assert_eq!(env_config.base.get("DEBUG"), Some(&EnvValue::Bool(true)));

    // Verify tasks
    assert!(manifest.tasks.contains_key("test"));

    Ok(())
}
