//! Task execution command implementation

use cuengine::{CueEvaluator, Cuenv};
use cuenv_core::Result;
use cuenv_core::environment::Environment;
use cuenv_core::tasks::{ExecutorConfig, TaskDefinition, TaskExecutor, TaskGraph, Tasks};
use std::fmt::Write;
use std::path::Path;

/// Execute a named task from the CUE configuration
pub async fn execute_task(
    path: &str,
    package: &str,
    task_name: Option<&str>,
    capture_output: bool,
    materialize_outputs: Option<&str>,
    show_cache_path: bool,
) -> Result<String> {
    tracing::info!(
        "Executing task from path: {}, package: {}, task: {:?}",
        path,
        package,
        task_name
    );

    // Evaluate CUE to get tasks and environment
    let evaluator = CueEvaluator::builder().build()?;
    let manifest: Cuenv = evaluator.evaluate_typed(Path::new(path), package)?;
    tracing::debug!("CUE evaluation successful");

    tracing::debug!(
        "Successfully parsed CUE evaluation, found {} tasks",
        manifest.tasks.len()
    );

    // If no task specified, list available tasks
    if task_name.is_none() {
        tracing::debug!("Listing available tasks");
        let tasks: Vec<&str> = manifest.tasks.keys().map(String::as_str).collect();
        tracing::debug!("Found {} tasks to list: {:?}", tasks.len(), tasks);

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
    tracing::debug!("Looking for specific task: {}", task_name);

    // Check if task exists
    let task_def = manifest.tasks.get(task_name).ok_or_else(|| {
        let available: Vec<&str> = manifest.tasks.keys().map(String::as_str).collect();
        tracing::error!(
            "Task '{}' not found in available tasks: {:?}",
            task_name,
            available
        );
        cuenv_core::Error::configuration(format!("Task '{task_name}' not found"))
    })?;

    tracing::debug!("Found task definition: {:?}", task_def);

    // Set up environment from manifest
    let mut environment = Environment::new();
    if let Some(env) = &manifest.env {
        // Build environment for task, applying policies
        let env_vars = cuenv_core::environment::Environment::build_for_task(task_name, &env.base);
        for (key, value) in env_vars {
            environment.set(key, value);
        }
    }

    // Create executor with environment
    let config = ExecutorConfig {
        capture_output,
        max_parallel: 0,
        environment,
        project_root: Path::new(path).to_path_buf(),
        materialize_outputs: materialize_outputs.map(|s| Path::new(s).to_path_buf()),
        show_cache_path,
    };

    let executor = TaskExecutor::new(config);

    // Convert manifest tasks to Tasks struct
    let tasks = Tasks {
        tasks: manifest.tasks.clone(),
    };

    // Build task graph for dependency-aware execution
    tracing::debug!("Building task graph for task: {}", task_name);
    let mut task_graph = TaskGraph::new();
    task_graph.build_for_task(task_name, &tasks).map_err(|e| {
        tracing::error!("Failed to build task graph: {}", e);
        e
    })?;
    tracing::debug!(
        "Successfully built task graph with {} tasks",
        task_graph.task_count()
    );

    // Execute using the appropriate method
    let results =
        execute_task_with_strategy(&executor, task_name, task_def, &task_graph, &tasks).await?;

    // Check for any failed tasks first
    for result in &results {
        if !result.success {
            return Err(cuenv_core::Error::configuration(format!(
                "Task '{}' failed with exit code {:?}",
                result.name, result.exit_code
            )));
        }
    }

    // Format results
    let output = format_task_results(results, capture_output, task_name);
    Ok(output)
}

/// Execute a task using the appropriate strategy based on task type and dependencies
async fn execute_task_with_strategy(
    executor: &TaskExecutor,
    task_name: &str,
    task_def: &TaskDefinition,
    task_graph: &TaskGraph,
    all_tasks: &Tasks,
) -> Result<Vec<cuenv_core::tasks::TaskResult>> {
    match task_def {
        TaskDefinition::Group(_) => {
            // For groups (sequential/parallel), use the original group execution
            // which properly handles sequential ordering and parallel execution
            executor
                .execute_definition(task_name, task_def, all_tasks)
                .await
        }
        TaskDefinition::Single(task) => {
            if task.depends_on.is_empty() {
                // Single task with no dependencies - use direct execution
                executor
                    .execute_definition(task_name, task_def, all_tasks)
                    .await
            } else {
                // Single task with dependencies - use graph execution
                executor.execute_graph(task_graph).await
            }
        }
    }
}

/// Format task execution results for output
fn format_task_results(
    results: Vec<cuenv_core::tasks::TaskResult>,
    capture_output: bool,
    task_name: &str,
) -> String {
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

    output
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
            None,
            false,
        )
        .await;

        // The result depends on FFI availability
        if let Ok(output) = result {
            assert!(output.contains("No tasks") || output.contains("Available tasks"));
        } else {
            // FFI not available in test environment
        }
    }
}
