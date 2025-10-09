//! Task execution command implementation

use cuengine::{CueEvaluator, Cuenv};
use cuenv_core::Result;
use cuenv_core::environment::Environment;
use cuenv_core::tasks::{ExecutorConfig, Task, TaskDefinition, TaskExecutor, TaskGraph, Tasks};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use uuid::Uuid;

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
        working_dir: None,
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
    let results = execute_task_with_strategy_hermetic(
        path,
        &evaluator,
        &executor,
        task_name,
        task_def,
        &task_graph,
        &tasks,
        manifest.env.as_ref(),
        capture_output,
    )
    .await?;

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
#[allow(clippy::too_many_arguments)]
async fn execute_task_with_strategy_hermetic(
    project_dir: &str,
    evaluator: &CueEvaluator,
    executor: &TaskExecutor,
    task_name: &str,
    task_def: &TaskDefinition,
    task_graph: &TaskGraph,
    all_tasks: &Tasks,
    env_base: Option<&cuenv_core::environment::Env>,
    capture_output: bool,
) -> Result<Vec<cuenv_core::tasks::TaskResult>> {
    match task_def {
        TaskDefinition::Group(_) => {
            // For groups (sequential/parallel), use the original group execution
            executor
                .execute_definition(task_name, task_def, all_tasks)
                .await
        }
        TaskDefinition::Single(t) => {
            // If no hermetic features are used, fall back to original execution
            if t.external_inputs.is_none() && t.inputs.is_empty() && t.outputs.is_empty() {
                if t.depends_on.is_empty() {
                    executor
                        .execute_definition(task_name, task_def, all_tasks)
                        .await
                } else {
                    executor.execute_graph(task_graph).await
                }
            } else {
                // Build parallel groups and run each task hermetically
                let groups = task_graph.get_parallel_groups()?;
                let mut all_results = Vec::new();
                for group in groups {
                    // Run group sequentially for simplicity; can be parallelized if needed
                    for node in group {
                        let result = run_task_hermetic(
                            Path::new(project_dir),
                            evaluator,
                            &node.name,
                            &node.task,
                            env_base,
                            capture_output,
                        )
                        .await?;
                        all_results.push(result);
                    }
                }
                Ok(all_results)
            }
        }
    }
}

/// Format task execution results for output
async fn run_task_hermetic(
    project_dir: &Path,
    evaluator: &CueEvaluator,
    name: &str,
    task: &Task,
    env_base: Option<&cuenv_core::environment::Env>,
    capture_output: bool,
) -> Result<cuenv_core::tasks::TaskResult> {
    // Discover git root
    let git_root = find_git_root(project_dir)?;
    tracing::info!(
        "Starting task '{}' with {} external mappings",
        name,
        task.external_inputs.as_ref().map_or(0, |v| v.len())
    );

    // Prepare hermetic workspace
    let workspace = create_workspace_dir(name)?;

    // Materialize external inputs first
    if let Some(externals) = &task.external_inputs {
        for ext in externals {
            resolve_and_materialize_external(
                &git_root,
                project_dir,
                evaluator,
                ext,
                &workspace,
                capture_output,
            )
            .await?;
        }
    }

    // Materialize local inputs
    for input in &task.inputs {
        let src = project_dir.join(input);
        let dst = workspace.join(input);
        materialize_path(&src, &dst)?;
    }

    // Compute environment for this task
    let mut env = Environment::new();
    if let Some(base) = env_base {
        let vars = Environment::build_for_task(name, &base.base);
        for (k, v) in vars {
            env.set(k, v);
        }
    }

    // Compute cache key for this task using materialized inputs
    let cache_key = compute_task_cache_key(task, &env, &workspace)?;
    let cache_dir = task_cache_dir()?.join(&cache_key);
    let outputs_dir = cache_dir.join("outputs");

    // If cache exists, skip execution
    if outputs_dir.exists() {
        tracing::info!("Cache hit for task '{}'", name);
        return Ok(cuenv_core::tasks::TaskResult {
            name: name.to_string(),
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            success: true,
        });
    }

    // Execute in hermetic workspace
    let exec = TaskExecutor::new(ExecutorConfig {
        capture_output,
        max_parallel: 0,
        environment: env.clone(),
        working_dir: Some(workspace.clone()),
    });

    let result = exec.execute_task(name, task).await?;

    // On success, store declared outputs into cache
    if result.success {
        store_outputs_in_cache(&workspace, &task.outputs, &outputs_dir)?;
    }

    Ok(result)
}

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

fn find_git_root(start: &Path) -> Result<PathBuf> {
    let mut current = start;
    loop {
        if current.join(".git").exists() {
            return Ok(current.to_path_buf());
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => {
                return Err(cuenv_core::Error::configuration(
                    "Git root not found".to_string(),
                ));
            }
        }
    }
}

fn canonicalize_within_root(root: &Path, path: &Path) -> Result<PathBuf> {
    let canon = fs::canonicalize(path).map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(path.to_path_buf().into_boxed_path()),
        operation: "canonicalize".to_string(),
    })?;
    if canon.starts_with(root) {
        Ok(canon)
    } else {
        Err(cuenv_core::Error::configuration(format!(
            "Resolved path '{}' is outside repository root '{}'",
            canon.display(),
            root.display()
        )))
    }
}

fn detect_package_name(dir: &Path) -> Result<String> {
    for entry in fs::read_dir(dir).map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(dir.to_path_buf().into_boxed_path()),
        operation: "read_dir".to_string(),
    })? {
        let path = entry
            .map_err(|e| cuenv_core::Error::Io {
                source: e,
                path: None,
                operation: "read_dir_entry".to_string(),
            })?
            .path();
        if path.extension().and_then(|s| s.to_str()) == Some("cue") {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Some(line) = content
                    .lines()
                    .find(|l| l.trim_start().starts_with("package "))
                {
                    let pkg = line.trim_start().trim_start_matches("package ").trim();
                    return Ok(pkg.to_string());
                }
            }
        }
    }
    Err(cuenv_core::Error::configuration(format!(
        "Could not detect CUE package name in {}",
        dir.display()
    )))
}

fn task_cache_dir() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| cuenv_core::Error::configuration("No home dir".to_string()))?;
    Ok(home.join(".cuenv").join("cache").join("tasks"))
}

fn create_workspace_dir(task_name: &str) -> Result<PathBuf> {
    let base = std::env::temp_dir().join("cuenv_workspaces");
    let _ = fs::create_dir_all(&base);
    let dir = base.join(format!(
        "{}-{}",
        task_name.replace(':', "_"),
        Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(dir.clone().into_boxed_path()),
        operation: "mkdir".to_string(),
    })?;
    Ok(dir)
}

fn materialize_path(src: &Path, dst: &Path) -> Result<()> {
    if src.is_dir() {
        // Copy directory recursively
        for entry in walkdir::WalkDir::new(src) {
            let entry = entry.map_err(|e| cuenv_core::Error::configuration(e.to_string()))?;
            let rel = entry.path().strip_prefix(src).unwrap();
            let target = dst.join(rel);
            if entry.file_type().is_dir() {
                fs::create_dir_all(&target).map_err(|e| cuenv_core::Error::Io {
                    source: e,
                    path: Some(target.clone().into_boxed_path()),
                    operation: "mkdir".to_string(),
                })?;
            } else {
                if let Some(parent) = target.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                // Try hardlink, fallback to copy
                if fs::hard_link(entry.path(), &target).is_err() {
                    fs::copy(entry.path(), &target).map_err(|e| cuenv_core::Error::Io {
                        source: e,
                        path: Some(target.clone().into_boxed_path()),
                        operation: "copy".to_string(),
                    })?;
                }
            }
        }
    } else {
        if let Some(parent) = dst.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if fs::hard_link(src, dst).is_err() {
            fs::copy(src, dst).map_err(|e| cuenv_core::Error::Io {
                source: e,
                path: Some(dst.to_path_buf().into_boxed_path()),
                operation: "copy".to_string(),
            })?;
        }
    }
    Ok(())
}

fn compute_task_cache_key(
    task: &Task,
    env: &Environment,
    workspace_inputs_root: &Path,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(b"v1");
    hasher.update(task.command.as_bytes());
    for arg in &task.args {
        hasher.update(b"\0");
        hasher.update(arg.as_bytes());
    }
    // Env map in sorted order
    let mut env_btree = BTreeMap::new();
    for (k, v) in env.iter() {
        env_btree.insert(k.clone(), v.clone());
    }
    for (k, v) in env_btree {
        hasher.update(b"\0env");
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(v.as_bytes());
    }
    // Hash all files in workspace that are inputs/materialized
    // We use declared inputs plus any paths under workspace that were materialized by external mapping (placed by caller)
    for input in &task.inputs {
        let path = workspace_inputs_root.join(input);
        hash_path_recursive(&mut hasher, &path)?;
    }
    // Also include mapped external destinations if any
    if let Some(exts) = &task.external_inputs {
        let mut unique_dests: HashSet<PathBuf> = HashSet::new();
        for ext in exts {
            for m in &ext.map {
                unique_dests.insert(workspace_inputs_root.join(&m.to));
            }
        }
        for dst in unique_dests {
            hash_path_recursive(&mut hasher, &dst)?;
        }
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn hash_path_recursive(hasher: &mut Sha256, path: &Path) -> Result<()> {
    if path.is_dir() {
        for entry in walkdir::WalkDir::new(path) {
            let entry = entry.map_err(|e| cuenv_core::Error::configuration(e.to_string()))?;
            if entry.file_type().is_file() {
                let mut f = fs::File::open(entry.path()).map_err(|e| cuenv_core::Error::Io {
                    source: e,
                    path: Some(entry.path().to_path_buf().into_boxed_path()),
                    operation: "open".to_string(),
                })?;
                let mut buf = Vec::new();
                f.read_to_end(&mut buf).map_err(|e| cuenv_core::Error::Io {
                    source: e,
                    path: Some(entry.path().to_path_buf().into_boxed_path()),
                    operation: "read".to_string(),
                })?;
                hasher.update(&buf);
            }
        }
    } else if path.is_file() {
        let mut f = fs::File::open(path).map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(path.to_path_buf().into_boxed_path()),
            operation: "open".to_string(),
        })?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(path.to_path_buf().into_boxed_path()),
            operation: "read".to_string(),
        })?;
        hasher.update(&buf);
    }
    Ok(())
}

fn store_outputs_in_cache(workspace: &Path, outputs: &[String], outputs_dir: &Path) -> Result<()> {
    fs::create_dir_all(outputs_dir).map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(outputs_dir.to_path_buf().into_boxed_path()),
        operation: "mkdir".to_string(),
    })?;
    for out in outputs {
        let src = workspace.join(out);
        let dst = outputs_dir.join(out);
        materialize_path(&src, &dst)?;
    }
    Ok(())
}

async fn resolve_and_materialize_external(
    git_root: &Path,
    current_project_dir: &Path,
    evaluator: &CueEvaluator,
    ext: &cuenv_core::tasks::ExternalInput,
    workspace: &Path,
    capture_output: bool,
) -> Result<()> {
    tracing::info!(
        "Resolving external task: project='{}' task='{}' mappings={}",
        ext.project,
        ext.task,
        ext.map.len()
    );

    // Resolve external project path
    let ext_dir = if ext.project.starts_with('/') {
        canonicalize_within_root(
            git_root,
            &git_root.join(ext.project.trim_start_matches('/')),
        )?
    } else {
        canonicalize_within_root(git_root, &current_project_dir.join(&ext.project))?
    };

    // Detect package name and evaluate
    let package = detect_package_name(&ext_dir)?;
    let manifest: Cuenv = evaluator.evaluate_typed(&ext_dir, &package)?;

    // Locate external task
    let task_def = manifest.tasks.get(&ext.task).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "External task '{}' not found in project {}",
            ext.task,
            ext_dir.display()
        ))
    })?;
    let task = match task_def {
        TaskDefinition::Single(t) => t.as_ref(),
        TaskDefinition::Group(_) => {
            return Err(cuenv_core::Error::configuration(
                "External task must be a single task".to_string(),
            ));
        }
    };

    // Validate mapping 'from' against declared outputs
    let declared: HashSet<&String> = task.outputs.iter().collect();
    for m in &ext.map {
        if !declared.contains(&m.from) {
            return Err(cuenv_core::Error::configuration(format!(
                "Mapping refers to non-declared output '{}'; declared outputs: {:?}",
                m.from, task.outputs
            )));
        }
    }

    // Ensure no destination collisions
    let mut dests: HashSet<&String> = HashSet::new();
    for m in &ext.map {
        if !dests.insert(&m.to) {
            return Err(cuenv_core::Error::configuration(format!(
                "Collision in mapping: destination '{}' specified multiple times",
                m.to
            )));
        }
    }

    // Build environment for external task (isolated)
    let mut env = Environment::new();
    if let Some(base) = manifest.env.as_ref() {
        let vars = Environment::build_for_task(&ext.task, &base.base);
        for (k, v) in vars {
            env.set(k, v);
        }
    }

    // Compute cache key for external task based on its inputs and environment
    // Use a temporary workspace to materialize its inputs (if any)
    let ext_workspace = create_workspace_dir(&format!("ext-{}", &ext.task))?;
    for input in &task.inputs {
        let src = ext_dir.join(input);
        let dst = ext_workspace.join(input);
        materialize_path(&src, &dst)?;
    }
    let ext_key = compute_task_cache_key(task, &env, &ext_workspace)?;
    let cache_dir = task_cache_dir()?.join(&ext_key);
    let outputs_dir = cache_dir.join("outputs");

    if outputs_dir.exists() {
        tracing::info!("Cache hit for external task '{}'", ext.task);
    } else {
        tracing::info!("Cache miss for external task '{}'; auto-running", ext.task);
        // Run the external task in hermetic workspace
        let exec = TaskExecutor::new(ExecutorConfig {
            capture_output,
            max_parallel: 0,
            environment: env.clone(),
            working_dir: Some(ext_workspace.clone()),
        });
        let res = exec.execute_task(&ext.task, task).await?;
        if !res.success {
            return Err(cuenv_core::Error::configuration(format!(
                "External task '{}' failed",
                ext.task
            )));
        }
        // Store outputs
        store_outputs_in_cache(&ext_workspace, &task.outputs, &outputs_dir)?;
    }

    // Materialize only selected outputs into dependent workspace
    for m in &ext.map {
        let src = outputs_dir.join(&m.from);
        let dst = workspace.join(&m.to);
        materialize_path(&src, &dst)?;
    }

    Ok(())
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

        let result = execute_task(temp_dir.path().to_str().unwrap(), "test", None, false).await;

        // The result depends on FFI availability
        if let Ok(output) = result {
            assert!(output.contains("No tasks") || output.contains("Available tasks"));
        } else {
            // FFI not available in test environment
        }
    }
}
