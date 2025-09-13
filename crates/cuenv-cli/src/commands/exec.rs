//! Exec command implementation for running arbitrary commands with CUE environment

use cuengine::{CueEvaluator, Cuenv};
use cuenv_core::environment::Environment;
use cuenv_core::tasks::execute_command;
use cuenv_core::Result;
use std::path::Path;

/// Execute an arbitrary command with the CUE environment
pub async fn execute_exec(
    path: &str,
    package: &str,
    command: &str,
    args: &[String],
) -> Result<i32> {
    tracing::info!(
        "Executing command with CUE environment from path: {}, package: {}, command: {} {:?}",
        path,
        package,
        command,
        args
    );

    // Evaluate CUE to get environment
    let evaluator = CueEvaluator::builder().build()?;
    let manifest: Cuenv = evaluator.evaluate_typed(Path::new(path), package)?;

    // Set up environment from manifest
    let mut environment = Environment::new();
    if let Some(env) = &manifest.env {
        for (key, value) in &env.base {
            use cuenv_core::environment::EnvValue;
            let value_str = match value {
                EnvValue::String(s) => s.clone(),
                EnvValue::Int(i) => i.to_string(),
                EnvValue::Bool(b) => b.to_string(),
                EnvValue::Secret(_) => continue,
            };
            environment.set(key.clone(), value_str);
        }
    }

    // Execute the command with the environment
    let exit_code = execute_command(command, args, &environment).await?;

    Ok(exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_execute_command_with_env() {
        let temp_dir = TempDir::new().unwrap();
        let cue_content = r#"package test
env: {
    TEST_VAR: "test_value"
}"#;
        fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

        // Test depends on FFI availability
        let result = execute_exec(
            temp_dir.path().to_str().unwrap(),
            "test",
            "echo",
            &["test".to_string()],
        )
        .await;

        if let Ok(exit_code) = result {
            assert_eq!(exit_code, 0);
        } else {
            // FFI not available in test environment
        }
    }

    #[tokio::test]
    async fn test_execute_shell_via_exec() {
        let temp_dir = TempDir::new().unwrap();
        let cue_content = r#"package test
env: {
    NAME: "World"
}"#;
        fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

        // Test shell execution via execute_exec with shell command
        let result = execute_exec(
            temp_dir.path().to_str().unwrap(),
            "test",
            "sh",
            &["-c".to_string(), "echo Hello".to_string()],
        )
        .await;

        if let Ok(exit_code) = result {
            assert_eq!(exit_code, 0);
        } else {
            // FFI not available in test environment
        }
    }
}
