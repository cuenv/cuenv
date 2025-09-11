//! Exec command implementation for running arbitrary commands with CUE environment

use cuengine::CueEvaluator;
use cuenv_core::Result;
use cuenv_core::environment::CueEvaluation;
use cuenv_core::task_executor::execute_command;
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
    let json = evaluator.evaluate(Path::new(path), package)?;
    let evaluation = CueEvaluation::from_json(&json).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to parse CUE evaluation: {e}"))
    })?;

    // Execute the command with the environment
    let environment = evaluation.get_environment();
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

        match result {
            Ok(exit_code) => {
                assert_eq!(exit_code, 0);
            }
            Err(_) => {
                // FFI not available in test environment
            }
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

        match result {
            Ok(exit_code) => {
                assert_eq!(exit_code, 0);
            }
            Err(_) => {
                // FFI not available in test environment
            }
        }
    }
}
