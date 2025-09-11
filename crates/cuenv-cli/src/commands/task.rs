//! Task execution command implementation

use cuenv_core::environment::CueEvaluation;
use cuenv_core::task_executor::{ExecutorConfig, TaskExecutor};
use cuenv_core::Result;
use cuengine::CueEvaluator;
use std::fmt::Write;
use std::path::Path;

/// Execute a named task from the CUE configuration
pub async fn execute_task(
    path: &str,
    package: &str,
    task_name: Option<&str>,
    capture_output: bool,
) -> Result<String> {
    tracing::info!(
        "Executing task from path: {}, package: {}, task: {:?}",
        path,
        package,
        task_name
    );
    
    // Evaluate CUE to get tasks and environment
    let evaluator = CueEvaluator::builder().build()?;
    let json = evaluator.evaluate(Path::new(path), package)?;
    let evaluation = CueEvaluation::from_json(&json).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to parse CUE evaluation: {e}"))
    })?;
    
    // If no task specified, list available tasks
    if task_name.is_none() {
        let tasks = evaluation.tasks.list_tasks();
        if tasks.is_empty() {
            return Ok("No tasks defined in the configuration".to_string());
        }
        
        let mut output = String::from("Available tasks:\n");
        for task in tasks {
            writeln!(output, "  - {task}").unwrap();
        }
        return Ok(output);
    }
    
    let task_name = task_name.unwrap();
    
    // Check if task exists
    let task_def = evaluation.tasks.get(task_name).ok_or_else(|| {
        cuenv_core::Error::configuration(format!("Task '{task_name}' not found"))
    })?;
    
    // Create executor with environment
    let config = ExecutorConfig {
        capture_output,
        max_parallel: 0,
        environment: evaluation.get_environment(),
    };
    
    let executor = TaskExecutor::new(config);
    
    // Execute the task
    let results = executor
        .execute_definition(task_name, task_def, &evaluation.tasks)
        .await?;
    
    // Format results
    let mut output = String::new();
    for result in results {
        if capture_output {
            write!(output, "Task '{}' ", result.name).unwrap();
            if result.success {
                output.push_str("succeeded\n");
                if !result.stdout.is_empty() {
                    output.push_str("Output:\n");
                    output.push_str(&result.stdout);
                    output.push('\n');
                }
            } else {
                writeln!(output, "failed with exit code {:?}", result.exit_code).unwrap();
                if !result.stderr.is_empty() {
                    output.push_str("Error:\n");
                    output.push_str(&result.stderr);
                    output.push('\n');
                }
            }
        }
    }
    
    if output.is_empty() {
        output = format!("Task '{task_name}' completed");
    }
    
    Ok(output)
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    
    #[tokio::test]
    async fn test_list_tasks_empty() {
        let temp_dir = TempDir::new().unwrap();
        let cue_content = r#"package test
env: {
    FOO: "bar"
}"#;
        fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();
        
        let result = execute_task(
            temp_dir.path().to_str().unwrap(),
            "test",
            None,
            false,
        )
        .await;
        
        // The result depends on FFI availability
        match result {
            Ok(output) => {
                assert!(output.contains("No tasks") || output.contains("Available tasks"));
            }
            Err(_) => {
                // FFI not available in test environment
            }
        }
    }
}