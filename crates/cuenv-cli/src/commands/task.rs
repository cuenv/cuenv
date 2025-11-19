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
use std::process::Command;
use uuid::Uuid;

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
    let manifest: Cuenv = evaluate_manifest_with_fallback(&evaluator, Path::new(path), package)?;
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
        // Build and resolve environment for task, applying policies and executing secret resolvers
        let env_vars =
            cuenv_core::environment::Environment::resolve_for_task(task_name, &env.base).await?;
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
        project_root: std::fs::canonicalize(path).unwrap_or_else(|_| Path::new(path).to_path_buf()),
        materialize_outputs: materialize_outputs.map(|s| Path::new(s).to_path_buf()),
        cache_dir: None,
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
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
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
            // If workspaceInputs is used, we MUST use the direct executor to access the real project root.
            // If no hermetic features are used, fall back to original execution.
            if t.workspace_inputs.is_some()
                || (t.external_inputs.is_none() && t.inputs.is_empty() && t.outputs.is_empty())
            {
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
        task.external_inputs.as_ref().map_or(0, Vec::len)
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
        let vars = Environment::resolve_for_task(name, &base.base).await?;
        for (k, v) in vars {
            env.set(k, v);
        }
    }

    // Augment inputs with mapped external destinations so they are included in
    // the hermetic input set and cache key computed by the core executor.
    let mut augmented_inputs = task.inputs.clone();
    if let Some(exts) = &task.external_inputs {
        for ext in exts {
            for m in &ext.map {
                if !augmented_inputs.contains(&m.to) {
                    augmented_inputs.push(m.to.clone());
                }
            }
        }
    }

    // Execute with project_root set to our prepared workspace so that the
    // executor resolves inputs from there (including external materials).
    let exec = TaskExecutor::new(ExecutorConfig {
        capture_output,
        max_parallel: 0,
        environment: env.clone(),
        working_dir: None,
        project_root: workspace.clone(),
        materialize_outputs: None,
        cache_dir: None,
        show_cache_path: false,
    });

    // Clone the task with augmented inputs
    let mut task_aug = task.clone();
    task_aug.inputs = augmented_inputs;

    let result = exec.execute_task(name, &task_aug).await?;

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
        } else {
            // When not capturing output, we still want to print cached logs on
            // cache hits (executor returns them in TaskResult). This ensures CLI
            // behavior matches a fresh execution where child output is inherited.
            if !result.stdout.is_empty() {
                output.push_str(&result.stdout);
                output.push('\n');
            }
        }
    }

    if capture_output && output.is_empty() {
        output = format!("Task '{task_name}' completed");
    } else if !capture_output {
        // In non-capturing mode, ensure we always include a clear completion
        // message even if we printed cached logs above.
        if output.is_empty() {
            output = format!("Task '{task_name}' completed");
        } else {
            let _ = writeln!(output, "Task '{task_name}' completed");
        }
    }

    output
}

fn find_git_root(start: &Path) -> Result<PathBuf> {
    let mut current = start;
    loop {
        if current.join(".git").exists() {
            // Canonicalize to resolve platform symlinks (e.g., macOS /var -> /private/var)
            // so subsequent path containment checks use a stable prefix.
            let canon = fs::canonicalize(current).unwrap_or_else(|_| current.to_path_buf());
            return Ok(canon);
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
    // Resolve both the root and candidate to canonical paths to avoid issues with
    // platform-specific symlinks (notably macOS's /var -> /private/var).
    let root_canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let canon = fs::canonicalize(path).map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(path.to_path_buf().into_boxed_path()),
        operation: "canonicalize".to_string(),
    })?;
    if canon.starts_with(&root_canon) {
        Ok(canon)
    } else {
        Err(cuenv_core::Error::configuration(format!(
            "Resolved path '{}' is outside repository root '{}'",
            canon.display(),
            root_canon.display()
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
        if path.extension().and_then(|s| s.to_str()) == Some("cue")
            && let Ok(content) = fs::read_to_string(&path)
            && let Some(line) = content
                .lines()
                .find(|l| l.trim_start().starts_with("package "))
        {
            let pkg = line.trim_start().trim_start_matches("package ").trim();
            return Ok(pkg.to_string());
        }
    }
    Err(cuenv_core::Error::configuration(format!(
        "Could not detect CUE package name in {}",
        dir.display()
    )))
}

fn evaluate_manifest_with_fallback(
    evaluator: &CueEvaluator,
    dir: &Path,
    package: &str,
) -> Result<Cuenv> {
    match evaluator.evaluate_typed(dir, package) {
        Ok(m) => Ok(m),
        Err(e) => {
            tracing::warn!(
                "FFI evaluation failed ({}); falling back to 'cue export'",
                e
            );
            // Fallback: use the `cue` CLI to export JSON and parse it
            // Allow overriding the cue binary via env for CI or non-standard setups
            let cue_bin = std::env::var("CUENV_CUE_BIN")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "cue".to_string());
            let output = Command::new(&cue_bin)
                .arg("export")
                .current_dir(dir)
                .arg(".")
                .output()
                .map_err(|ioe| cuenv_core::Error::Io {
                    source: ioe,
                    path: Some(dir.to_path_buf().into_boxed_path()),
                    operation: format!("{cue_bin} export"),
                })?;

            if !output.status.success() {
                // Provide a clearer hint when the cue binary is missing or fails
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let hint = format!(
                    "Fallback CUE CLI evaluation failed. Ensure the Go FFI bridge is available or install the 'cue' binary and set CUENV_CUE_BIN if needed. Tried binary: '{cue_bin}'."
                );
                return Err(cuenv_core::Error::configuration(format!(
                    "'{cue_bin} export' failed: {stderr}\n{hint}"
                )));
            }

            let json_str = String::from_utf8_lossy(&output.stdout).to_string();

            serde_json::from_str::<Cuenv>(&json_str).map_err(|e| {
                cuenv_core::Error::configuration(format!(
                    "Failed to parse CUE JSON from fallback: {e}"
                ))
            })
        }
    }
}

#[allow(dead_code)]
fn task_cache_dir() -> PathBuf {
    // Use OS temp dir for cache to ensure write access in sandboxed/test environments.
    // This avoids relying on HOME/XDG locations that may be unavailable in Nix builds.
    std::env::temp_dir()
        .join(".cuenv")
        .join("cache")
        .join("tasks")
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(clippy::too_many_lines)]
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
    let manifest: Cuenv = evaluate_manifest_with_fallback(evaluator, &ext_dir, &package)?;

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

    // Compute cache key exactly as core executor does
    let input_resolver = cuenv_core::tasks::io::InputResolver::new(&ext_dir);
    let resolved_inputs = input_resolver.resolve(&task.inputs)?;
    let inputs_summary = resolved_inputs.to_summary_map();
    let mut env_summary = BTreeMap::new();
    for (k, v) in env.iter() {
        env_summary.insert(k.clone(), v.clone());
    }
    let cuenv_version = cuenv_core::VERSION.to_string();
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let shell_json = serde_json::to_value(&task.shell).ok();
    let envelope = cuenv_core::cache::tasks::CacheKeyEnvelope {
        inputs: inputs_summary,
        command: task.command.clone(),
        args: task.args.clone(),
        shell: shell_json,
        env: env_summary,
        cuenv_version,
        platform,
        workspace_lockfile_hash: None,
        workspace_package_hashes: None,
    };
    let (ext_key, _env_json) = cuenv_core::cache::tasks::compute_cache_key(&envelope)?;

    // Ensure cache exists (run if miss)
    if cuenv_core::cache::tasks::lookup(&ext_key, None).is_none() {
        tracing::info!(
            "Cache miss for external task '{}' (key {})",
            ext.task,
            ext_key
        );
        let exec = TaskExecutor::new(ExecutorConfig {
            capture_output,
            max_parallel: 0,
            environment: env.clone(),
            working_dir: None,
            project_root: ext_dir.clone(),
            materialize_outputs: None,
            cache_dir: None,
            show_cache_path: false,
        });
        let res = exec.execute_task(&ext.task, task).await?;
        if !res.success {
            return Err(cuenv_core::Error::configuration(format!(
                "External task '{}' failed",
                ext.task
            )));
        }
    } else {
        tracing::info!("Cache hit for external task '{}'", ext.task);
    }

    // Materialize selected outputs from cache into dependent workspace
    let mat_dir = std::env::temp_dir().join("cuenv_ext_mat").join(&ext_key);
    let _ = fs::remove_dir_all(&mat_dir);
    fs::create_dir_all(&mat_dir).ok();
    let _ = cuenv_core::cache::tasks::materialize_outputs(&ext_key, &mat_dir, None)?;
    for m in &ext.map {
        let src = mat_dir.join(&m.from);
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

    #[test]
    fn test_find_git_root_success_and_failure() {
        let tmp = TempDir::new().unwrap();
        // failure: no .git
        let err = find_git_root(tmp.path()).expect_err("should fail without .git");
        let _ = err; // silence unused

        // success: with .git at parent
        let proj = tmp.path().join("proj");
        fs::create_dir_all(proj.join("sub")).unwrap();
        fs::create_dir_all(tmp.path().join(".git")).unwrap();
        let root = find_git_root(&proj).expect("should locate git root");
        // canonicalization should normalize to tmp root
        assert_eq!(root, std::fs::canonicalize(tmp.path()).unwrap());
    }

    #[test]
    fn test_canonicalize_within_root_ok_and_reject_outside() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        let inside = root.join("a/b");
        fs::create_dir_all(&inside).unwrap();
        let ok = canonicalize_within_root(root, &inside).expect("inside should be ok");
        let root_canon = std::fs::canonicalize(root).unwrap();
        assert!(ok.starts_with(&root_canon));

        // outside: a sibling temp dir
        let other = TempDir::new().unwrap();
        let outside = other.path().join("x");
        fs::create_dir_all(&outside).unwrap();
        let err = canonicalize_within_root(root, &outside).expect_err("should reject outside");
        let _ = err; // silence unused
    }

    #[test]
    fn test_detect_package_name_ok_and_err() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        // ok
        fs::write(dir.join("env.cue"), "package mypkg\n// rest").unwrap();
        let pkg = detect_package_name(dir).expect("should detect package");
        assert_eq!(pkg, "mypkg");

        // err: empty dir
        let empty = TempDir::new().unwrap();
        let err = detect_package_name(empty.path()).expect_err("no package should error");
        let _ = err;
    }

    #[test]
    fn test_create_workspace_and_materialize_path() {
        // create workspace
        let dir = create_workspace_dir("task:name").expect("workspace created");
        assert!(dir.exists());

        // materialize file
        let tmp = TempDir::new().unwrap();
        let src_file = tmp.path().join("data.txt");
        fs::write(&src_file, "hello").unwrap();
        let dst_file = dir.join("copy/data.txt");
        materialize_path(&src_file, &dst_file).expect("materialize file");
        assert_eq!(fs::read_to_string(&dst_file).unwrap(), "hello");

        // materialize directory
        let src_dir = tmp.path().join("tree");
        fs::create_dir_all(src_dir.join("nested")).unwrap();
        fs::write(src_dir.join("nested/file.txt"), "content").unwrap();
        let dst_dir = dir.join("tree_copy");
        materialize_path(&src_dir, &dst_dir).expect("materialize dir");
        assert_eq!(
            fs::read_to_string(dst_dir.join("nested/file.txt")).unwrap(),
            "content"
        );
    }

    #[test]
    fn test_compute_task_cache_key_changes_on_input_change() {
        let tmp = TempDir::new().unwrap();
        // prepare a workspace inputs root
        let ws = tmp.path().join("ws");
        fs::create_dir_all(ws.join("inputs")).unwrap();
        fs::write(ws.join("inputs/a.txt"), "A").unwrap();

        let mut env = Environment::new();
        env.set("FOO".into(), "1".into());
        let task = Task {
            command: "echo".into(),
            args: vec!["hi".into()],
            shell: None,
            env: std::collections::HashMap::default(),
            depends_on: vec![],
            inputs: vec!["inputs".into()],
            outputs: vec![],
            external_inputs: None,
            workspace_inputs: None,
            description: None,
        };

        let k1 = compute_task_cache_key(&task, &env, &ws).expect("key1");
        // mutate input
        fs::write(ws.join("inputs/a.txt"), "B").unwrap();
        let k2 = compute_task_cache_key(&task, &env, &ws).expect("key2");
        assert_ne!(k1, k2, "key should change when input changes");
    }

    #[test]
    fn test_format_task_results_variants() {
        let r_ok = cuenv_core::tasks::TaskResult {
            name: "t".into(),
            exit_code: Some(0),
            stdout: "hello".into(),
            stderr: String::new(),
            success: true,
        };
        let r_fail = cuenv_core::tasks::TaskResult {
            name: "t".into(),
            exit_code: Some(1),
            stdout: String::new(),
            stderr: "boom".into(),
            success: false,
        };

        // capture on: show status and fields
        let s = format_task_results(vec![r_ok.clone(), r_fail.clone()], true, "t");
        assert!(s.contains("succeeded"));
        assert!(s.contains("Output:"));
        assert!(s.contains("failed with exit code"));
        assert!(s.contains("Error:"));

        // capture off: logs passed through + completion line
        let s2 = format_task_results(vec![r_ok], false, "t");
        assert!(s2.contains("hello"));
        assert!(s2.contains("Task 't' completed"));

        // capture on with empty output -> default completion
        let s3 = format_task_results(vec![], true, "abc");
        assert_eq!(s3, "Task 'abc' completed");
    }

    #[tokio::test]
    async fn test_run_task_hermetic_local_inputs_and_outputs() {
        // project dir with .git and local inputs
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(proj.join(".git")).unwrap();
        fs::create_dir_all(proj.join("inputs")).unwrap();
        fs::write(proj.join("inputs/in.txt"), "data").unwrap();

        let evaluator = cuengine::CueEvaluator::builder()
            .no_retry()
            .build()
            .unwrap();

        let task = Task {
            command: "sh".into(),
            args: vec![
                "-c".into(),
                "mkdir -p out; cat inputs/in.txt > out/out.txt; echo done".into(),
            ],
            shell: None,
            env: std::collections::HashMap::default(),
            depends_on: vec![],
            inputs: vec!["inputs".into()],
            outputs: vec!["out/out.txt".into()],
            external_inputs: None,
            workspace_inputs: None,
            description: None,
        };

        // Execute directly via helper
        let res = run_task_hermetic(&proj, &evaluator, "mytask", &task, None, true)
            .await
            .expect("hermetic run ok");
        assert!(res.success);
        assert!(res.stdout.contains("done"));
    }

    #[tokio::test]
    async fn test_execute_task_with_strategy_hermetic_single_task() {
        // Build a single task that declares inputs/outputs to trigger hermetic path
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(proj.join(".git")).unwrap();
        fs::create_dir_all(proj.join("inputs")).unwrap();
        fs::write(proj.join("inputs/in.txt"), "x").unwrap();

        let evaluator = cuengine::CueEvaluator::builder()
            .no_retry()
            .build()
            .unwrap();
        let exec = TaskExecutor::new(ExecutorConfig {
            capture_output: true,
            ..Default::default()
        });

        let task = Task {
            command: "sh".into(),
            args: vec!["-c".into(), "cp inputs/in.txt out.txt".into()],
            shell: None,
            env: std::collections::HashMap::default(),
            depends_on: vec![],
            inputs: vec!["inputs".into()],
            outputs: vec!["out.txt".into()],
            external_inputs: None,
            workspace_inputs: None,
            description: None,
        };
        let def = TaskDefinition::Single(Box::new(task.clone()));
        let mut all = Tasks::new();
        all.tasks.insert("copy".into(), def.clone());
        let mut g = TaskGraph::new();
        g.build_from_definition("copy", &def, &all).unwrap();

        let results = execute_task_with_strategy_hermetic(
            proj.to_str().unwrap(),
            &evaluator,
            &exec,
            "copy",
            &def,
            &g,
            &all,
            None,
            true,
        )
        .await
        .expect("execute hermetic ok");
        assert_eq!(results.len(), 1);
        assert!(results[0].success);
    }
}
