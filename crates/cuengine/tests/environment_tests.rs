//! Tests for CUE evaluation and JSON parsing

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

    let json =
        cuengine::evaluate_cue_package(&project_root.join("examples/task-basic"), "examples")?;
    let project: Project = serde_json::from_str(&json)?;
    let mut env = Environment::new();
    if let Some(env_config) = &project.env {
        for (key, value) in &env_config.base {
            env.set(key.clone(), value.to_string_value());
        }
    }

    assert_eq!(env.get("NAME"), Some("Jack O'Neill"));
    assert!(project.tasks.contains_key("interpolate"));
    assert!(project.tasks.contains_key("propagate"));
    assert!(project.tasks.contains_key("greetAll"));
    assert!(project.tasks.contains_key("shellExample"));

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

    let json = cuengine::evaluate_cue_package(temp_dir.path(), "cuenv")?;

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
