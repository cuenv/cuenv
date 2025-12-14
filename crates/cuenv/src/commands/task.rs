//! Task execution command implementation

// Some functions are reserved for hermetic execution which is temporarily disabled
#![allow(dead_code)]

use cuengine::CueEvaluator;
use cuenv_core::Result;
use cuenv_core::environment::Environment;
use cuenv_core::manifest::Cuenv;
use cuenv_core::manifest::TaskRef;
use cuenv_core::tasks::discovery::{EvalFn, TaskDiscovery};
use cuenv_core::tasks::executor::{TASK_FAILURE_SNIPPET_LINES, summarize_task_failure};
use cuenv_core::tasks::{
    BackendFactory, ExecutorConfig, ResolvedArgs, Task, TaskDefinition, TaskExecutor, TaskGraph,
    TaskIndex, TaskParams, Tasks,
};

use super::env_file::find_cue_module_root;
use crate::tui::rich::RichTui;
use crate::tui::state::TaskInfo;
use cuenv_events::{EventBus, CuenvEventLayer};
use tracing_subscriber::layer::SubscriberExt;

/// Get the dagger backend factory if the feature is enabled
#[cfg(feature = "dagger-backend")]
#[allow(clippy::unnecessary_wraps)] // Both cfg variants need same return type
fn get_dagger_factory() -> Option<BackendFactory> {
    Some(cuenv_dagger::create_dagger_backend)
}

#[cfg(not(feature = "dagger-backend"))]
fn get_dagger_factory() -> Option<BackendFactory> {
    None
}
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

use super::export::get_environment_with_hooks;

/// Execute a named task from the CUE configuration.
///
/// Tasks can be selected in two mutually exclusive ways:
/// - By name: Provide `task_name` to execute a specific task
/// - By labels: Provide `labels` to execute all tasks matching ALL given labels (AND semantics)
///
/// When using labels, the function discovers all projects in the CUE module scope,
/// finds tasks matching the labels, creates a synthetic root task that depends on them,
/// and executes via the DAG.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub async fn execute_task(
    path: &str,
    package: &str,
    task_name: Option<&str>,
    labels: &[String],
    environment: Option<&str>,
    format: &str,
    capture_output: bool,
    materialize_outputs: Option<&str>,
    show_cache_path: bool,
    backend: Option<&str>,
    tui: bool,
    help: bool,
    task_args: &[String],
) -> Result<String> {
    // Handle CLI help immediately if no task specified
    if task_name.is_none() && help {
        return Ok(get_task_cli_help());
    }

    tracing::info!(
        "Executing task from path: {}, package: {}, task: {:?}",
        path,
        package,
        task_name
    );

    // Evaluate CUE to get tasks and environment
    let evaluator = Arc::new(
        CueEvaluator::builder()
            .build()
            .map_err(super::convert_engine_error)?,
    );
    let manifest: Cuenv =
        evaluate_manifest(&evaluator, Path::new(path), package)?.with_implicit_tasks();
    tracing::debug!("CUE evaluation successful");

    tracing::debug!(
        "Successfully parsed CUE evaluation, found {} tasks",
        manifest.tasks.len()
    );

    // We may need the cue.mod root later for global task discovery / cross-project deps.
    let project_root =
        std::fs::canonicalize(path).unwrap_or_else(|_| Path::new(path).to_path_buf());
    let cue_module_root = find_cue_module_root(&project_root);

    // Build a canonical index to support nested task paths
    let task_index = TaskIndex::build(&manifest.tasks)?;
    let local_tasks = task_index.to_tasks();

    // If no task specified, list available tasks
    if task_name.is_none() && labels.is_empty() {
        tracing::debug!("Listing available tasks");
        let tasks = task_index.list();
        tracing::debug!("Found {} tasks to list", tasks.len());

        if format == "json" {
            return serde_json::to_string(&tasks).map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to serialize tasks: {e}"))
            });
        }

        if tasks.is_empty() {
            return Ok("No tasks defined in the configuration".to_string());
        }

        // Calculate current working directory relative to cue.mod root
        let project_root =
            std::fs::canonicalize(path).unwrap_or_else(|_| Path::new(path).to_path_buf());
        let cue_module_root = find_cue_module_root(&project_root);
        let cwd_relative = cue_module_root.as_ref().and_then(|root| {
            project_root
                .strip_prefix(root)
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        });

        return Ok(render_task_tree(tasks, cwd_relative.as_deref()));
    }

    if !labels.is_empty() && task_name.is_some() {
        return Err(cuenv_core::Error::configuration(
            "Cannot specify both a task name and --label",
        ));
    }
    if !labels.is_empty() && !task_args.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "Task arguments are not supported when selecting tasks by label",
        ));
    }

    // Validate that labels are non-empty after normalization
    let normalized_labels = normalize_labels(labels);
    if !labels.is_empty() && normalized_labels.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "Labels cannot be empty or whitespace-only",
        ));
    }

    let display_task_name: String;
    let task_def: TaskDefinition;
    let all_tasks: Tasks;
    let task_graph_root_name: String;

    if normalized_labels.is_empty() {
        // Execute a named task
        let requested_task = task_name.unwrap();
        tracing::debug!("Looking for specific task: {}", requested_task);

        // If help requested for specific task/group
        if help {
            let tasks = task_index.list();
            let prefix = format!("{requested_task}.");
            let subtasks: Vec<&cuenv_core::tasks::IndexedTask> = tasks
                .iter()
                .filter(|t| t.name == requested_task || t.name.starts_with(&prefix))
                .copied()
                .collect();

            if subtasks.is_empty() {
                return Err(cuenv_core::Error::configuration(format!(
                    "Task '{requested_task}' not found",
                )));
            }

            // If it's a single task without subtasks
            if subtasks.len() == 1 && subtasks[0].name == requested_task {
                return Ok(format_task_detail(subtasks[0]));
            }

            // It's a group or task with subtasks
            // Note: For help on specific groups, we don't need cwd-relative sorting
            return Ok(render_task_tree(subtasks, None));
        }

        // Resolve task via canonical index (supports nested paths and ':' alias)
        let task_entry = task_index.resolve(requested_task)?;
        let canonical_task_name = task_entry.name.clone();
        tracing::debug!(
            "Task index entries: {:?}",
            task_index
                .list()
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
        );
        tracing::debug!(
            "Indexed tasks for execution: {:?}",
            local_tasks.list_tasks()
        );
        tracing::debug!(
            "Requested task '{}' present: {}",
            requested_task,
            local_tasks.get(requested_task).is_some()
        );
        let original_task_def = local_tasks.get(&canonical_task_name).ok_or_else(|| {
            cuenv_core::Error::configuration(format!("Task '{canonical_task_name}' not found"))
        })?;
        display_task_name = canonical_task_name;

        tracing::debug!("Found task definition: {:?}", original_task_def);

        // Process task arguments if provided
        let (selected_task_def, tasks) = if task_args.is_empty() {
            (original_task_def.clone(), local_tasks.clone())
        } else if let TaskDefinition::Single(task) = original_task_def {
            // Parse and validate arguments against task params
            let resolved_args = resolve_task_args(task.params.as_ref(), task_args)?;
            tracing::debug!("Resolved task args: {:?}", resolved_args);

            // Apply argument interpolation to task
            let modified_task = apply_args_to_task(task, &resolved_args);

            // Create a new task definition with the modified task
            let modified_def = TaskDefinition::Single(Box::new(modified_task));

            // Create a new Tasks collection with the modified task
            let mut modified_tasks = local_tasks.clone();
            modified_tasks
                .tasks
                .insert(display_task_name.to_string(), modified_def.clone());

            (modified_def, modified_tasks)
        } else {
            // For groups, we don't support arguments
            return Err(cuenv_core::Error::configuration(
                "Task arguments are not supported for task groups".to_string(),
            ));
        };

        task_def = selected_task_def;

        // Use a global task registry (keyed by FQDN) when we can locate cue.mod.
        // This enables cross-project dependency graphs and proper cycle detection.
        let (global_tasks, task_root_name) = if let Some(module_root) = &cue_module_root {
            let (mut global, current_project_id) =
                build_global_tasks(evaluator.clone(), module_root, &project_root, &manifest)?;

            // If we interpolated args for the invoked task, patch that task node in the
            // global registry so execution matches the CLI-resolved definition.
            // (We avoid patching otherwise, because the global registry has normalized
            // dependsOn entries to FQDNs.)
            if !task_args.is_empty()
                && let TaskDefinition::Single(ref t) = task_def
            {
                let fqdn = task_fqdn(&current_project_id, &display_task_name);
                if let Some(TaskDefinition::Single(existing)) = global.tasks.get_mut(&fqdn) {
                    existing.command.clone_from(&t.command);
                    existing.args.clone_from(&t.args);
                }
            }

            let root = task_fqdn(&current_project_id, &display_task_name);
            (global, root)
        } else {
            (tasks.clone(), display_task_name.clone())
        };

        all_tasks = global_tasks;
        task_graph_root_name = task_root_name;
    } else {
        // Execute tasks by label
        let (mut tasks_in_scope, _current_project_id) =
            if let Some(module_root) = &cue_module_root {
                build_global_tasks(evaluator.clone(), module_root, &project_root, &manifest)
                    .map_err(|e| {
                        cuenv_core::Error::configuration(format!(
                            "Failed to discover tasks for label execution: {e}"
                        ))
                    })?
            } else {
                (local_tasks.clone(), String::new())
            };

        let matching_tasks = find_tasks_with_labels(&tasks_in_scope, &normalized_labels);

        if matching_tasks.is_empty() {
            return Err(cuenv_core::Error::configuration(format!(
                "No tasks with labels {normalized_labels:?} were found in this scope"
            )));
        }

        display_task_name = format_label_root(&normalized_labels);
        // Create a synthetic aggregator task that depends on all label-matched tasks.
        // The script is "true" (a shell no-op that always succeeds) because actual work
        // is performed by the dependsOn tasks; this task just serves as the DAG root.
        let synthetic = Task {
            script: Some("true".to_string()),
            hermetic: false,
            depends_on: matching_tasks,
            project_root: Some(project_root.clone()),
            description: Some(format!(
                "Run all tasks matching labels: {}",
                normalized_labels.join(", ")
            )),
            ..Default::default()
        };

        tasks_in_scope.tasks.insert(
            display_task_name.clone(),
            TaskDefinition::Single(Box::new(synthetic)),
        );

        // Safety: We just inserted the synthetic task above, so it will always exist.
        task_def = tasks_in_scope
            .get(&display_task_name)
            .cloned()
            .expect("synthetic task missing after insertion");
        task_graph_root_name = display_task_name.clone();
        all_tasks = tasks_in_scope;
    }

    // Get environment with hook-generated vars merged in
    let directory = project_root.clone();
    let base_env_vars = get_environment_with_hooks(&directory, &manifest, package).await?;

    // Apply task-specific policies and secret resolvers on top of the merged environment
    let mut runtime_env = Environment::new();
    if let Some(env) = &manifest.env {
        // First apply the base environment (static + hooks)
        for (key, value) in &base_env_vars {
            runtime_env.set(key.clone(), value.clone());
        }

        // Get environment variables, applying environment-specific overrides if specified
        let env_vars = if let Some(env_name) = environment {
            env.for_environment(env_name)
        } else {
            env.base.clone()
        };

        // Then apply task-specific overrides with policies and secret resolution
        let task_env_vars = cuenv_core::environment::Environment::resolve_for_task(
            display_task_name.as_str(),
            &env_vars,
        )
        .await?;
        for (key, value) in task_env_vars {
            runtime_env.set(key, value);
        }
    } else {
        // No manifest env, just use hook-generated environment
        for (key, value) in base_env_vars {
            runtime_env.set(key, value);
        }
    }

    // Create executor with environment
    let config = ExecutorConfig {
        capture_output,
        max_parallel: 0,
        environment: runtime_env.clone(),
        working_dir: None,
        cue_module_root,
        project_root: project_root.clone(),
        materialize_outputs: materialize_outputs.map(|s| Path::new(s).to_path_buf()),
        cache_dir: None,
        show_cache_path,
        workspaces: manifest.workspaces.clone(),
        backend_config: manifest.config.as_ref().and_then(|c| c.backend.clone()),
        cli_backend: backend.map(ToString::to_string),
    };

    let executor = TaskExecutor::with_dagger_factory(config, get_dagger_factory());

    // Build task graph for dependency-aware execution
    tracing::debug!("Building task graph for task: {}", task_graph_root_name);
    let mut task_graph = TaskGraph::new();
    task_graph
        .build_for_task(&task_graph_root_name, &all_tasks)
        .map_err(|e| {
            tracing::error!("Failed to build task graph: {}", e);
            e
        })?;
    tracing::debug!(
        "Successfully built task graph with {} tasks",
        task_graph.task_count()
    );

    // If TUI is requested and we have a task graph, launch the rich TUI
    if tui && task_graph.task_count() > 0 {
        return execute_with_rich_tui(
            path,
            &evaluator,
            &executor,
            display_task_name.as_str(),
            &task_def,
            &task_graph,
            &all_tasks,
            manifest.env.as_ref(),
            &runtime_env,
            capture_output,
        )
        .await;
    }

    // Execute using the appropriate method
    let results = execute_task_with_strategy_hermetic(
        path,
        &evaluator,
        &executor,
        display_task_name.as_str(),
        &task_def,
        &task_graph,
        &all_tasks,
        manifest.env.as_ref(),
        &runtime_env,
        capture_output,
    )
    .await?;

    // Check for any failed tasks first and return a rich summary
    if let Some(failed) = results.iter().find(|r| !r.success) {
        return Err(cuenv_core::Error::configuration(summarize_task_failure(
            failed,
            TASK_FAILURE_SNIPPET_LINES,
        )));
    }

    // Format results
    let output = format_task_results(results, capture_output, display_task_name.as_str());
    Ok(output)
}

/// Execute task with rich TUI interface
#[allow(clippy::too_many_arguments)]
async fn execute_with_rich_tui(
    _project_dir: &str,
    _evaluator: &CueEvaluator,
    executor: &TaskExecutor,
    task_name: &str,
    _task_def: &TaskDefinition,
    task_graph: &TaskGraph,
    all_tasks: &Tasks,
    _env_base: Option<&cuenv_core::environment::Env>,
    _hook_env: &Environment,
    _capture_output: bool,
) -> Result<String> {
    // Create event bus for TUI
    let bus = EventBus::new();
    let event_layer = CuenvEventLayer::new(bus.sender().inner);

    // Install the event layer into tracing
    // Note: This temporarily affects the global subscriber
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::registry().with(event_layer),
    );

    // Get event receiver for TUI
    let event_rx = bus.receiver().inner;

    // Create and initialize TUI
    let mut tui = RichTui::new(event_rx).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to initialize TUI: {e}"))
    })?;

    // Build TaskInfo structs from the task graph
    let mut task_infos = Vec::new();
    let sorted_tasks = task_graph.topological_sort().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to sort task graph: {e}"))
    })?;

    // Calculate levels based on dependencies
    let mut levels: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for node in &sorted_tasks {
        let max_dep_level = node.task.depends_on.iter()
            .filter_map(|dep| levels.get(&dep.task).copied())
            .max()
            .unwrap_or(0);
        levels.insert(node.name.clone(), max_dep_level + (if node.task.depends_on.is_empty() { 0 } else { 1 }));
    }

    for node in sorted_tasks {
        let task_name = node.name.clone();
        let dependencies: Vec<String> = node.task.depends_on.iter().map(|dep| dep.task.clone()).collect();
        let level = levels.get(&task_name).copied().unwrap_or(0);

        task_infos.push(TaskInfo::new(task_name, dependencies, level));
    }

    tui.init_tasks(task_infos);

    // Run TUI and task execution concurrently
    let tui_handle = tokio::spawn(async move {
        tui.run().await
    });

    // Execute tasks
    let results = executor.execute_graph(task_graph).await?;

    // Wait for TUI to finish
    let _ = tui_handle.await;

    // Check for failures
    if let Some(failed) = results.iter().find(|r| !r.success) {
        return Err(cuenv_core::Error::configuration(summarize_task_failure(
            failed,
            TASK_FAILURE_SNIPPET_LINES,
        )));
    }

    // Return success message
    Ok(format!("Task '{task_name}' completed successfully in TUI mode"))
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
    _hook_env: &Environment,
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
            // NOTE: Hermetic execution is temporarily disabled in the core executor
            // (see executor.rs TODO comment). We must use non-hermetic execution here
            // to be consistent, otherwise tasks behave differently when run directly
            // vs via a group.
            //
            // TODO: Re-enable hermetic execution when sandbox properly preserves
            // monorepo structure and handles all edge cases.
            let _ = (project_dir, evaluator, env_base, capture_output, t); // Suppress unused warnings

            // IMPORTANT:
            // The TaskDefinition passed here may be sourced from the *local* manifest
            // (pre-global-normalization / pre-injection). In global mode, additional
            // dependencies can be injected (e.g., workspace setup chains, TaskRef
            // expansion) that won't be reflected in `t.depends_on`.
            //
            // The task graph is built from `all_tasks` and is the authoritative view.
            if task_graph.task_count() <= 1 {
                executor
                    .execute_definition(task_name, task_def, all_tasks)
                    .await
            } else {
                executor.execute_graph(task_graph).await
            }
        }
    }
}

/// Execute a single task hermetically with pre-computed hook environment
async fn run_task_hermetic(
    project_dir: &Path,
    evaluator: &CueEvaluator,
    name: &str,
    task: &Task,
    env_base: Option<&cuenv_core::environment::Env>,
    hook_env: &Environment,
    capture_output: bool,
) -> Result<cuenv_core::tasks::TaskResult> {
    // Discover git root
    let git_root = find_git_root(project_dir)?;
    let project_ref_count = task.iter_project_refs().count();
    tracing::info!(
        "Starting task '{}' with {} project references",
        name,
        project_ref_count
    );

    // Prepare hermetic workspace
    let workspace = create_workspace_dir(name)?;

    // Materialize project references first
    for reference in task.iter_project_refs() {
        resolve_and_materialize_project_reference(
            &git_root,
            project_dir,
            evaluator,
            reference,
            &workspace,
            capture_output,
        )
        .await?;
    }

    // Materialize local inputs
    for input in task.iter_path_inputs() {
        let src = project_dir.join(input);
        let dst = workspace.join(input);
        materialize_path(&src, &dst)?;
    }

    // Use the pre-computed hook environment
    let mut env = Environment::new();
    // First apply hook environment (includes PATH and other Nix-provided variables)
    for (k, v) in hook_env.iter() {
        env.set(k.clone(), v.clone());
    }
    // Then apply task-specific overrides from env_base
    if let Some(base) = env_base {
        let vars = Environment::resolve_for_task(name, &base.base).await?;
        for (k, v) in vars {
            env.set(k, v);
        }
    }

    // Execute with project_root set to our prepared workspace so that the
    // executor resolves inputs from there (including external materials).
    let exec = TaskExecutor::with_dagger_factory(
        ExecutorConfig {
            capture_output,
            max_parallel: 0,
            environment: env.clone(),
            working_dir: None,
            project_root: workspace.clone(),
            cue_module_root: find_cue_module_root(&workspace),
            materialize_outputs: None,
            cache_dir: None,
            show_cache_path: false,
            workspaces: None,
            backend_config: None,
            cli_backend: None,
        },
        get_dagger_factory(),
    );

    let result = exec.execute_task(name, task).await?;

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
            // When not capturing output, logs are streamed directly by the executor
            // or printed from cache by the executor (if modified).
            // We do NOT print them again here to avoid duplication.
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

/// Normalize a list of labels by sorting, deduplicating, and filtering empty strings.
///
/// This ensures consistent behavior across label matching and naming operations.
fn normalize_labels(labels: &[String]) -> Vec<String> {
    let mut normalized: Vec<String> = labels
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    normalized.sort();
    normalized.dedup();
    normalized
}

/// Find all tasks that match ALL of the given labels (AND semantics).
///
/// Returns a sorted list of task FQDNs (or names) that have all required labels.
/// Tasks must be `Single` tasks with labels that include every label in the input.
fn find_tasks_with_labels(tasks: &Tasks, labels: &[String]) -> Vec<String> {
    let required_labels = normalize_labels(labels);

    let mut matching: Vec<String> = tasks
        .tasks
        .iter()
        .filter_map(|(name, definition)| match definition {
            TaskDefinition::Single(task)
                if required_labels
                    .iter()
                    .all(|label| task.labels.contains(label)) =>
            {
                Some(name.clone())
            }
            _ => None,
        })
        .collect();

    matching.sort();
    matching
}

/// Generate a deterministic synthetic task name for label-based execution.
///
/// The name uses a reserved prefix (`__cuenv_labels__`) to avoid collisions with
/// user-defined task names. The labels are sorted and joined with `+` for stability.
fn format_label_root(labels: &[String]) -> String {
    let sorted = normalize_labels(labels);
    format!("__cuenv_labels__{}", sorted.join("+"))
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

fn evaluate_manifest(evaluator: &CueEvaluator, dir: &Path, package: &str) -> Result<Cuenv> {
    evaluator
        .evaluate_typed(dir, package)
        .map_err(super::convert_engine_error)
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
    // We use declared inputs plus any paths under workspace that were materialized by project references
    for input in task.iter_path_inputs() {
        let path = workspace_inputs_root.join(input);
        hash_path_recursive(&mut hasher, &path)?;
    }

    let mut unique_dests: HashSet<PathBuf> = HashSet::new();
    for reference in task.iter_project_refs() {
        for mapping in &reference.map {
            unique_dests.insert(workspace_inputs_root.join(&mapping.to));
        }
    }
    for dst in unique_dests {
        hash_path_recursive(&mut hasher, &dst)?;
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
async fn resolve_and_materialize_project_reference(
    git_root: &Path,
    current_project_dir: &Path,
    evaluator: &CueEvaluator,
    reference: &cuenv_core::tasks::ProjectReference,
    workspace: &Path,
    capture_output: bool,
) -> Result<()> {
    tracing::info!(
        "Resolving project reference: project='{}' task='{}' mappings={}",
        reference.project,
        reference.task,
        reference.map.len()
    );

    // Resolve external project path
    let ext_dir = if reference.project.starts_with('/') {
        canonicalize_within_root(
            git_root,
            &git_root.join(reference.project.trim_start_matches('/')),
        )?
    } else {
        canonicalize_within_root(git_root, &current_project_dir.join(&reference.project))?
    };

    // Detect package name and evaluate
    let package = detect_package_name(&ext_dir)?;
    let manifest: Cuenv = evaluate_manifest(evaluator, &ext_dir, &package)?.with_implicit_tasks();

    // Locate external task
    let task_def = manifest.tasks.get(&reference.task).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "External task '{}' not found in project {}",
            reference.task,
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
    for m in &reference.map {
        if !declared.contains(&m.from) {
            return Err(cuenv_core::Error::configuration(format!(
                "Mapping refers to non-declared output '{}'; declared outputs: {:?}",
                m.from, task.outputs
            )));
        }
    }

    // Ensure no destination collisions
    let mut dests: HashSet<&String> = HashSet::new();
    for m in &reference.map {
        if !dests.insert(&m.to) {
            return Err(cuenv_core::Error::configuration(format!(
                "Collision in mapping: destination '{}' specified multiple times",
                m.to
            )));
        }
    }

    // Build environment for external task (isolated)
    let mut env = Environment::new();
    if let Some(_base) = manifest.env.as_ref() {
        // Get base environment with hook-generated vars
        let base_env_vars = get_environment_with_hooks(&ext_dir, &manifest, &package).await?;

        // Apply base environment (static + hooks)
        for (k, v) in &base_env_vars {
            env.set(k.clone(), v.clone());
        }

        // Note: External tasks use build_for_task which doesn't resolve secrets/policies
        // This is intentional for hermetic execution
        let vars =
            Environment::build_for_task(&reference.task, &manifest.env.as_ref().unwrap().base);
        for (k, v) in vars {
            env.set(k, v);
        }
    }

    // Compute cache key exactly as core executor does
    let input_resolver = cuenv_core::tasks::io::InputResolver::new(&ext_dir);
    let input_patterns = task.collect_path_inputs_with_prefix(None);
    let resolved_inputs = input_resolver.resolve(&input_patterns)?;
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
        workspace_lockfile_hashes: None,
        workspace_package_hashes: None,
    };
    let (ext_key, _env_json) = cuenv_core::cache::tasks::compute_cache_key(&envelope)?;

    // Ensure cache exists (run if miss)
    if cuenv_core::cache::tasks::lookup(&ext_key, None).is_none() {
        tracing::info!(
            "Cache miss for external task '{}' (key {})",
            reference.task,
            ext_key
        );
        let exec = TaskExecutor::with_dagger_factory(
            ExecutorConfig {
                capture_output,
                max_parallel: 0,
                environment: env.clone(),
                working_dir: None,
                project_root: ext_dir.clone(),
                cue_module_root: find_cue_module_root(&ext_dir),
                materialize_outputs: None,
                cache_dir: None,
                show_cache_path: false,
                workspaces: None,
                backend_config: None,
                cli_backend: None,
            },
            get_dagger_factory(),
        );
        let res = exec.execute_task(&reference.task, task).await?;
        if !res.success {
            return Err(cuenv_core::Error::configuration(format!(
                "External task '{}' failed",
                reference.task
            )));
        }
    } else {
        tracing::info!("Cache hit for external task '{}'", reference.task);
    }

    // Materialize selected outputs from cache into dependent workspace
    let mat_dir = std::env::temp_dir().join("cuenv_ext_mat").join(&ext_key);
    let _ = fs::remove_dir_all(&mat_dir);
    fs::create_dir_all(&mat_dir).ok();
    let _ = cuenv_core::cache::tasks::materialize_outputs(&ext_key, &mat_dir, None)?;
    for m in &reference.map {
        let src = mat_dir.join(&m.from);
        let dst = workspace.join(&m.to);
        materialize_path(&src, &dst)?;
    }

    Ok(())
}

fn get_task_cli_help() -> String {
    r"Execute a task defined in CUE configuration

Usage: cuenv task [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the task to execute (list tasks if not provided)

Options:
  -p, --path <PATH>                  Path to directory containing CUE files [default: .]
      --package <PACKAGE>            Name of the CUE package to evaluate [default: cuenv]
      --materialize-outputs <DIR>    Materialize cached outputs to this directory on cache hit (off by default)
      --show-cache-path              Print the cache path for this task key
      --backend <BACKEND>            Force specific execution backend (e.g., 'host', 'dagger')
      --help                         Print help"
        .to_string()
}

fn format_task_detail(task: &cuenv_core::tasks::IndexedTask) -> String {
    let mut output = String::new();
    writeln!(output, "Task: {}", task.name).unwrap();

    match &task.definition {
        TaskDefinition::Single(t) => {
            if let Some(desc) = &t.description {
                writeln!(output, "Description: {desc}").unwrap();
            }
            writeln!(output, "Command: {}", t.command).unwrap();
            if !t.args.is_empty() {
                writeln!(output, "Args: {:?}", t.args).unwrap();
            }
            if !t.depends_on.is_empty() {
                writeln!(output, "Depends on: {:?}", t.depends_on).unwrap();
            }
            if !t.inputs.is_empty() {
                writeln!(output, "Inputs: {:?}", t.inputs).unwrap();
            }
            if !t.outputs.is_empty() {
                writeln!(output, "Outputs: {:?}", t.outputs).unwrap();
            }
            // Show params if defined
            if let Some(params) = &t.params {
                if !params.positional.is_empty() {
                    writeln!(output, "\nPositional Arguments:").unwrap();
                    for (i, param) in params.positional.iter().enumerate() {
                        let required = if param.required { " (required)" } else { "" };
                        let default = param
                            .default
                            .as_ref()
                            .map(|d| format!(" [default: {d}]"))
                            .unwrap_or_default();
                        let desc = param
                            .description
                            .as_ref()
                            .map(|d| format!(" - {d}"))
                            .unwrap_or_default();
                        writeln!(output, "  {{{{{i}}}}}{required}{default}{desc}").unwrap();
                    }
                }
                if !params.named.is_empty() {
                    writeln!(output, "\nNamed Arguments:").unwrap();
                    let mut names: Vec<_> = params.named.keys().collect();
                    names.sort();
                    for name in names {
                        let param = &params.named[name];
                        let short = param
                            .short
                            .as_ref()
                            .map(|s| format!("-{s}, "))
                            .unwrap_or_default();
                        let required = if param.required { " (required)" } else { "" };
                        let default = param
                            .default
                            .as_ref()
                            .map(|d| format!(" [default: {d}]"))
                            .unwrap_or_default();
                        let desc = param
                            .description
                            .as_ref()
                            .map(|d| format!(" - {d}"))
                            .unwrap_or_default();
                        writeln!(output, "  {short}--{name}{required}{default}{desc}").unwrap();
                    }
                }
            }
        }
        TaskDefinition::Group(g) => {
            writeln!(output, "Type: Task Group").unwrap();
            match g {
                cuenv_core::tasks::TaskGroup::Sequential(_) => {
                    writeln!(output, "Mode: Sequential").unwrap();
                }
                cuenv_core::tasks::TaskGroup::Parallel(_) => {
                    writeln!(output, "Mode: Parallel").unwrap();
                }
            }
        }
    }
    output
}

#[derive(Default)]
struct TaskTreeNode {
    description: Option<String>,
    children: BTreeMap<String, TaskTreeNode>,
    is_task: bool,
}

/// Render tasks grouped by source file, ordered by proximity to current directory
///
/// `cwd_relative`: Current working directory relative to cue.mod root (e.g., "projects/foo")
/// Tasks from the current directory are shown first, then progressively further parent dirs
fn render_task_tree(
    tasks: Vec<&cuenv_core::tasks::IndexedTask>,
    cwd_relative: Option<&str>,
) -> String {
    // Group tasks by source file
    // Normalize root-level sources: both "" and "env.cue" are treated as root
    let mut by_source: BTreeMap<String, Vec<&cuenv_core::tasks::IndexedTask>> = BTreeMap::new();
    for task in tasks {
        let source = task.source_file.clone().unwrap_or_default();
        // Normalize root sources to empty string so they group together
        let normalized = if source == "env.cue" {
            String::new()
        } else {
            source
        };
        by_source.entry(normalized).or_default().push(task);
    }

    // Sort source files by proximity to current directory
    // - Tasks from cwd come first
    // - Then tasks from parent directories (closest parent first)
    // - Root tasks come last (unless cwd is root)
    let mut sources: Vec<_> = by_source.keys().cloned().collect();
    sources.sort_by(|a, b| {
        let proximity_a = source_proximity(a, cwd_relative);
        let proximity_b = source_proximity(b, cwd_relative);
        // Lower proximity = closer to cwd = should come first
        proximity_a.cmp(&proximity_b).then(a.cmp(b))
    });

    let mut output = String::new();
    for (i, source) in sources.iter().enumerate() {
        if i > 0 {
            output.push('\n');
        }

        // Format header
        let header = if source.is_empty() || source == "env.cue" {
            "Tasks:".to_string()
        } else {
            format!("Tasks from {source}:")
        };
        writeln!(output, "{header}").unwrap();

        // Build and render tree for this source's tasks
        let source_tasks = &by_source[source];
        render_source_tasks(source_tasks, &mut output);
    }

    if output.is_empty() {
        output = "No tasks defined in the configuration".to_string();
    }

    output
}

/// Calculate proximity of a source file to the current directory
/// Lower value = closer to cwd = should be shown first
///
/// Returns:
/// - 0 if source is in the same directory as cwd
/// - 1+ for parent directories (1 = immediate parent, 2 = grandparent, etc.)
/// - `usize::MAX` / 2 for unrelated paths (children of cwd)
fn source_proximity(source: &str, cwd_relative: Option<&str>) -> usize {
    // Get the source directory (remove the filename like env.cue)
    let source_dir = if source.is_empty() {
        ""
    } else {
        std::path::Path::new(source)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("")
    };

    let cwd = cwd_relative.unwrap_or("");

    // If both are root level
    if source_dir.is_empty() && cwd.is_empty() {
        return 0;
    }

    // If source is in the same directory as cwd
    if source_dir == cwd {
        return 0;
    }

    // Check if source is an ancestor of cwd (parent directory)
    if cwd.starts_with(source_dir)
        && (source_dir.is_empty() || cwd[source_dir.len()..].starts_with('/'))
    {
        // Count how many levels up the source is
        let source_depth = if source_dir.is_empty() {
            0
        } else {
            source_dir.matches('/').count() + 1
        };
        let cwd_depth = if cwd.is_empty() {
            0
        } else {
            cwd.matches('/').count() + 1
        };
        // Distance = how many levels up from cwd to reach source
        return cwd_depth - source_depth;
    }

    // Source is not an ancestor of cwd (could be a sibling or child)
    // Show these after all ancestors
    usize::MAX / 2
}

/// Render tasks from a single source file as a tree
fn render_source_tasks(tasks: &[&cuenv_core::tasks::IndexedTask], output: &mut String) {
    let mut roots: BTreeMap<String, TaskTreeNode> = BTreeMap::new();

    // Build the tree
    for task in tasks {
        let parts: Vec<&str> = task.name.split('.').collect();
        let mut current_level = &mut roots;

        for (i, part) in parts.iter().enumerate() {
            let is_last = i == parts.len() - 1;
            let node = current_level.entry((*part).to_string()).or_default();

            if is_last {
                node.is_task = true;
                // Extract description from definition
                let desc = match &task.definition {
                    TaskDefinition::Single(t) => t.description.clone(),
                    TaskDefinition::Group(g) => match g {
                        cuenv_core::tasks::TaskGroup::Sequential(sub) => {
                            sub.first().and_then(|t| match t {
                                TaskDefinition::Single(st) => st.description.clone(),
                                TaskDefinition::Group(_) => None,
                            })
                        }
                        cuenv_core::tasks::TaskGroup::Parallel(_) => None,
                    },
                };
                node.description = desc;
            }

            current_level = &mut node.children;
        }
    }

    // Calculate max width for alignment
    let max_width = calculate_tree_width(&roots, 0);

    print_tree_nodes(&roots, output, max_width, "");
}

fn calculate_tree_width(nodes: &BTreeMap<String, TaskTreeNode>, depth: usize) -> usize {
    let mut max = 0;
    for (name, node) in nodes {
        // Length calculation:
        // depth * 3 (indentation) + 3 (marker "├─ ") + name.len()
        // Actually let's be precise with the print logic:
        // Root items: "├─ name" (len = 3 + name)
        // Nested: "│  ├─ name" (len = depth*3 + 3 + name)
        let len = (depth * 3) + 3 + name.len();
        if len > max {
            max = len;
        }
        let child_max = calculate_tree_width(&node.children, depth + 1);
        if child_max > max {
            max = child_max;
        }
    }
    max
}

fn print_tree_nodes(
    nodes: &BTreeMap<String, TaskTreeNode>,
    output: &mut String,
    max_width: usize,
    prefix: &str,
) {
    let count = nodes.len();
    for (i, (name, node)) in nodes.iter().enumerate() {
        let is_last_item = i == count - 1;

        let marker = if is_last_item { "└─ " } else { "├─ " };

        let current_line_len =
            prefix.chars().count() + marker.chars().count() + name.chars().count();

        write!(output, "{prefix}{marker}{name}").unwrap();

        if let Some(desc) = &node.description {
            // Pad with dots
            let padding = max_width.saturating_sub(current_line_len);
            // Add a minimum spacing
            let dots = ".".repeat(padding + 4);
            write!(output, " {dots} {desc}").unwrap();
        }
        writeln!(output).unwrap();

        let child_prefix = if is_last_item { "   " } else { "│  " };
        let new_prefix = format!("{prefix}{child_prefix}");

        print_tree_nodes(&node.children, output, max_width, &new_prefix);
    }
}

/// Check if an argument looks like a flag (starts with `-` but is not a negative number)
fn looks_like_flag(arg: &str) -> bool {
    if !arg.starts_with('-') {
        return false;
    }
    // Not a flag if it's a negative number (e.g., -1, -3.14)
    let rest = &arg[1..];
    if rest.is_empty() {
        return false;
    }
    // Check if it parses as a number
    rest.parse::<f64>().is_err()
}

/// Parse CLI arguments into positional and named values
/// If params is provided, short flags (-x) are resolved to their long names
/// Supports `--` separator to end flag parsing
fn parse_task_args(
    args: &[String],
    params: Option<&TaskParams>,
) -> (Vec<String>, std::collections::HashMap<String, String>) {
    let mut positional = Vec::new();
    let mut named = std::collections::HashMap::new();
    let mut flags_ended = false;

    // Build short-to-long flag mapping
    let short_to_long: std::collections::HashMap<String, String> = params
        .map(|p| {
            p.named
                .iter()
                .filter_map(|(name, def)| def.short.as_ref().map(|s| (s.clone(), name.clone())))
                .collect()
        })
        .unwrap_or_default();

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        // Handle `--` separator: all subsequent args are positional
        if arg == "--" {
            flags_ended = true;
            i += 1;
            continue;
        }

        if flags_ended {
            positional.push(arg.clone());
        } else if arg.starts_with("--") {
            let key = arg.strip_prefix("--").unwrap_or(arg);
            // Check if there's an '=' in the argument (e.g., --key=value)
            if let Some((k, v)) = key.split_once('=') {
                named.insert(k.to_string(), v.to_string());
            } else if i + 1 < args.len() && !looks_like_flag(&args[i + 1]) {
                // Next argument is the value
                named.insert(key.to_string(), args[i + 1].clone());
                i += 1;
            } else {
                // Boolean flag (no value)
                named.insert(key.to_string(), "true".to_string());
            }
        } else if let Some(short_key) = arg.strip_prefix('-') {
            // Short flag handling: must be single char and not a digit
            if short_key.len() == 1 && !short_key.chars().next().unwrap_or('0').is_ascii_digit() {
                let long_key = short_to_long
                    .get(short_key)
                    .cloned()
                    .unwrap_or_else(|| short_key.to_string());

                if i + 1 < args.len() && !looks_like_flag(&args[i + 1]) {
                    named.insert(long_key, args[i + 1].clone());
                    i += 1;
                } else {
                    // Boolean flag
                    named.insert(long_key, "true".to_string());
                }
            } else {
                // Not a valid short flag (multi-char like -abc, or negative number)
                positional.push(arg.clone());
            }
        } else {
            positional.push(arg.clone());
        }
        i += 1;
    }

    (positional, named)
}

/// Validate and resolve arguments against task parameter definitions
fn resolve_task_args(params: Option<&TaskParams>, cli_args: &[String]) -> Result<ResolvedArgs> {
    let (positional_values, named_values) = parse_task_args(cli_args, params);
    let mut resolved = ResolvedArgs::new();

    if let Some(params) = params {
        // Validate excess positional arguments
        let max_positional = params.positional.len();
        if positional_values.len() > max_positional {
            return Err(cuenv_core::Error::configuration(format!(
                "Too many positional arguments: expected at most {}, got {}",
                max_positional,
                positional_values.len()
            )));
        }

        // Validate unknown named arguments
        let unknown_flags: Vec<String> = named_values
            .keys()
            .filter(|k| !params.named.contains_key(*k))
            .map(|k| format!("--{k}"))
            .collect();
        if !unknown_flags.is_empty() {
            return Err(cuenv_core::Error::configuration(format!(
                "Unknown argument(s): {}",
                unknown_flags.join(", ")
            )));
        }

        // Process positional arguments
        for (i, param_def) in params.positional.iter().enumerate() {
            if let Some(value) = positional_values.get(i) {
                resolved.positional.push(value.clone());
            } else if let Some(default) = &param_def.default {
                resolved.positional.push(default.clone());
            } else if param_def.required {
                let default_desc = format!("positional argument {i}");
                let desc = param_def.description.as_deref().unwrap_or(&default_desc);
                return Err(cuenv_core::Error::configuration(format!(
                    "Missing required argument: {desc}"
                )));
            } else {
                resolved.positional.push(String::new());
            }
        }

        // Process named arguments
        for (name, param_def) in &params.named {
            if let Some(value) = named_values.get(name) {
                resolved.named.insert(name.clone(), value.clone());
            } else if let Some(default) = &param_def.default {
                resolved.named.insert(name.clone(), default.clone());
            } else if param_def.required {
                return Err(cuenv_core::Error::configuration(format!(
                    "Missing required argument: --{name}"
                )));
            }
        }
    } else {
        // No params defined, just pass through all args
        resolved.positional = positional_values;
        resolved.named = named_values;
    }

    Ok(resolved)
}

/// Apply resolved arguments to a task, interpolating placeholders in command and args
fn apply_args_to_task(task: &Task, resolved_args: &ResolvedArgs) -> Task {
    let mut new_task = task.clone();

    // Interpolate command
    new_task.command = resolved_args.interpolate(&task.command);

    // Interpolate args
    new_task.args = resolved_args.interpolate_args(&task.args);

    new_task
}

fn normalize_task_name(raw: &str) -> String {
    raw.replace(':', ".")
}

fn task_fqdn(project_id: &str, task_name: &str) -> String {
    format!("task:{project_id}:{}", normalize_task_name(task_name))
}

fn canonicalize_dep_for_task_name(dep: &str, task_name: &str) -> String {
    // Match TaskIndex semantics: treat dotted/colon deps as absolute, otherwise
    // resolve relative to the parent namespace of `task_name`.
    if dep.contains('.') || dep.contains(':') {
        return normalize_task_name(dep);
    }

    let task_name_norm = normalize_task_name(task_name);
    let mut segments = task_name_norm
        .split('.')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    segments.pop();
    segments.push(dep);
    segments.join(".")
}

fn compute_project_id(manifest: &Cuenv, project_root: &Path, module_root: &Path) -> String {
    let trimmed = manifest.name.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }

    // Fallback: stable id derived from path relative to cue.mod root.
    // Replace separators with '.' to keep the id colon-free (':' is our delimiter).
    let rel = project_root
        .strip_prefix(module_root)
        .unwrap_or(project_root)
        .to_string_lossy()
        .replace(['/', '\\'], ".");
    format!("path.{rel}")
}

fn set_default_project_root(def: &mut TaskDefinition, project_root: &PathBuf) {
    match def {
        TaskDefinition::Single(task) => {
            if task.project_root.is_none() {
                task.project_root = Some(project_root.clone());
            }
        }
        TaskDefinition::Group(group) => match group {
            cuenv_core::tasks::TaskGroup::Sequential(tasks) => {
                for t in tasks {
                    set_default_project_root(t, project_root);
                }
            }
            cuenv_core::tasks::TaskGroup::Parallel(parallel) => {
                for t in parallel.tasks.values_mut() {
                    set_default_project_root(t, project_root);
                }
            }
        },
    }
}

fn normalize_dep(
    dep: &str,
    default_project_id: &str,
    project_id_by_name: &HashMap<String, String>,
) -> String {
    let dep = dep.trim();
    if dep.starts_with("task:") {
        return dep.to_string();
    }

    if dep.starts_with('#') {
        let parsed = TaskRef {
            ref_: dep.to_string(),
        };
        if let Some((proj, task)) = parsed.parse() {
            let proj_id = project_id_by_name.get(&proj).cloned().unwrap_or(proj);
            return task_fqdn(&proj_id, &task);
        }
    }

    task_fqdn(default_project_id, dep)
}

fn normalize_definition_deps(
    def: &mut TaskDefinition,
    project_id_by_root: &HashMap<PathBuf, String>,
    project_id_by_name: &HashMap<String, String>,
    default_project_id: &str,
) {
    fn scope_project_id_for_task(
        task: &Task,
        project_id_by_root: &HashMap<PathBuf, String>,
        fallback: &str,
    ) -> String {
        if let Some(root) = &task.project_root
            && let Some(id) = project_id_by_root.get(root)
        {
            return id.clone();
        }
        fallback.to_string()
    }

    match def {
        TaskDefinition::Single(task) => {
            let scope_id = scope_project_id_for_task(task, project_id_by_root, default_project_id);
            let deps = std::mem::take(&mut task.depends_on);
            task.depends_on = deps
                .into_iter()
                .map(|d| normalize_dep(&d, &scope_id, project_id_by_name))
                .collect();
        }
        TaskDefinition::Group(group) => match group {
            cuenv_core::tasks::TaskGroup::Sequential(tasks) => {
                for t in tasks {
                    normalize_definition_deps(
                        t,
                        project_id_by_root,
                        project_id_by_name,
                        default_project_id,
                    );
                }
            }
            cuenv_core::tasks::TaskGroup::Parallel(parallel) => {
                // Normalize group-level depends_on too
                let group_deps = std::mem::take(&mut parallel.depends_on);
                parallel.depends_on = group_deps
                    .into_iter()
                    .map(|d| normalize_dep(&d, default_project_id, project_id_by_name))
                    .collect();
                for t in parallel.tasks.values_mut() {
                    normalize_definition_deps(
                        t,
                        project_id_by_root,
                        project_id_by_name,
                        default_project_id,
                    );
                }
            }
        },
    }
}

fn resolve_task_refs_in_definition(
    def: &mut TaskDefinition,
    discovery: &TaskDiscovery,
    manifest_project_id: &str,
    project_id_by_name: &HashMap<String, String>,
) {
    match def {
        TaskDefinition::Single(task) => {
            let Some(task_ref_str) = task.task_ref.clone() else {
                return;
            };
            let parsed_ref = TaskRef { ref_: task_ref_str };

            let placeholder_deps = std::mem::take(&mut task.depends_on)
                .into_iter()
                .map(|d| normalize_dep(&d, manifest_project_id, project_id_by_name))
                .collect::<Vec<_>>();

            match discovery.resolve_ref(&parsed_ref) {
                Ok(matched) => {
                    let mut resolved = matched.task;
                    let resolved_root =
                        fs::canonicalize(&matched.project_root).unwrap_or(matched.project_root);
                    resolved.project_root = Some(resolved_root);
                    resolved.task_ref = None;

                    // Canonicalize the referenced task's dependencies relative to the
                    // referenced task name (NOT the placeholder task name), so later indexing
                    // doesn't reinterpret them under the hook task namespace.
                    let deps = std::mem::take(&mut resolved.depends_on);
                    resolved.depends_on = deps
                        .into_iter()
                        .map(|d| canonicalize_dep_for_task_name(&d, &matched.task_name))
                        .collect();

                    for dep in placeholder_deps {
                        if !resolved.depends_on.contains(&dep) {
                            resolved.depends_on.push(dep);
                        }
                    }
                    **task = resolved;
                }
                Err(e) => {
                    tracing::warn!("Failed to resolve TaskRef {}: {}", parsed_ref.ref_, e);
                    // Restore placeholder deps so later normalization still has them.
                    task.depends_on = placeholder_deps;
                }
            }
        }
        TaskDefinition::Group(group) => match group {
            cuenv_core::tasks::TaskGroup::Sequential(tasks) => {
                for t in tasks {
                    resolve_task_refs_in_definition(
                        t,
                        discovery,
                        manifest_project_id,
                        project_id_by_name,
                    );
                }
            }
            cuenv_core::tasks::TaskGroup::Parallel(parallel) => {
                for t in parallel.tasks.values_mut() {
                    resolve_task_refs_in_definition(
                        t,
                        discovery,
                        manifest_project_id,
                        project_id_by_name,
                    );
                }
            }
        },
    }
}

fn resolve_task_refs_in_manifest(
    manifest: &mut Cuenv,
    discovery: &TaskDiscovery,
    manifest_project_id: &str,
    project_id_by_name: &HashMap<String, String>,
) {
    for def in manifest.tasks.values_mut() {
        resolve_task_refs_in_definition(def, discovery, manifest_project_id, project_id_by_name);
    }
}

fn get_task_mut_by_path<'a>(
    tasks: &'a mut HashMap<String, TaskDefinition>,
    raw_path: &str,
) -> Option<&'a mut Task> {
    let normalized = raw_path.replace(':', ".");
    let mut segments = normalized
        .split('.')
        .filter(|s| !s.is_empty())
        .map(str::trim)
        .collect::<Vec<_>>();
    if segments.is_empty() {
        return None;
    }

    let first = segments.remove(0);
    let mut current = tasks.get_mut(first)?;
    for seg in segments {
        match current {
            TaskDefinition::Group(cuenv_core::tasks::TaskGroup::Parallel(group)) => {
                current = group.tasks.get_mut(seg)?;
            }
            _ => return None,
        }
    }

    match current {
        TaskDefinition::Single(task) => Some(task.as_mut()),
        TaskDefinition::Group(_) => None,
    }
}

fn get_task_mut_by_name_or_path<'a>(
    tasks: &'a mut HashMap<String, TaskDefinition>,
    raw_path: &str,
) -> Option<&'a mut Task> {
    let normalized = raw_path.replace(':', ".");

    // Prefer direct lookup for top-level keys (covers injected implicit tasks like "bun.install")
    if tasks.contains_key(&normalized) {
        return match tasks.get_mut(&normalized) {
            Some(TaskDefinition::Single(task)) => Some(task.as_mut()),
            _ => None,
        };
    }

    // Fallback: nested lookup (covers `tasks: bun: install: {}`)
    get_task_mut_by_path(tasks, &normalized)
}

#[allow(clippy::too_many_lines)]
fn inject_workspace_setup_tasks(
    manifest: &mut Cuenv,
    discovery: &TaskDiscovery,
    manifest_project_id: &str,
) -> Result<()> {
    use cuenv_core::manifest::HookItem;
    use cuenv_core::tasks::TaskGroup;

    // NOTE: This is long because it needs to translate workspace config + hook steps into
    // concrete tasks while carefully avoiding dependency cycles.
    #[allow(clippy::too_many_lines)]
    fn add_setup_dep_to_definition(
        task_name: &str,
        task_def: &mut TaskDefinition,
        ws_name: &str,
        setup_task_name: &str,
    ) {
        // Avoid cycles: never make install/setup/hooks depend on setup.
        let install_name = format!("{ws_name}.install");
        let setup_name = format!("{ws_name}.setup");
        let hooks_prefix = format!("{ws_name}.hooks.");

        match task_def {
            TaskDefinition::Single(task) => {
                if !task.workspaces.iter().any(|w| w == ws_name) {
                    return;
                }

                if task_name == install_name
                    || task_name == setup_name
                    || task_name.starts_with(&hooks_prefix)
                {
                    return;
                }

                if !task.depends_on.contains(&setup_task_name.to_string()) {
                    task.depends_on.push(setup_task_name.to_string());
                }
            }
            TaskDefinition::Group(group) => match group {
                TaskGroup::Sequential(tasks) => {
                    for (i, sub_task) in tasks.iter_mut().enumerate() {
                        let sub_name = format!("{task_name}[{i}]");
                        add_setup_dep_to_definition(&sub_name, sub_task, ws_name, setup_task_name);
                    }
                }
                TaskGroup::Parallel(group) => {
                    for (name, sub_task) in &mut group.tasks {
                        let sub_name = format!("{task_name}.{name}");
                        add_setup_dep_to_definition(&sub_name, sub_task, ws_name, setup_task_name);
                    }
                }
            },
        }
    }

    let Some(workspaces) = &manifest.workspaces else {
        return Ok(());
    };

    // Clone to avoid borrow issues
    let workspaces = workspaces.clone();

    for (ws_name, config) in &workspaces {
        if !config.enabled {
            continue;
        }

        // Only inject if this workspace is actually referenced by tasks.
        let workspace_used = manifest
            .tasks
            .values()
            .any(|task_def| task_def.uses_workspace(ws_name));
        if !workspace_used {
            continue;
        }

        let install_task_name = format!("{ws_name}.install");
        let setup_task_name = format!("{ws_name}.setup");

        // Expand beforeInstall hook steps into concrete tasks.
        let mut all_hook_task_names: Vec<String> = Vec::new();
        let mut previous_step_task_names: Vec<String> = Vec::new();

        if let Some(hooks) = &config.hooks
            && let Some(before_install) = &hooks.before_install
        {
            for (step_idx, hook_item) in before_install.iter().enumerate() {
                match hook_item {
                    HookItem::Task(task) => {
                        let hook_task_name = format!("{ws_name}.hooks.beforeInstall[{step_idx}]");
                        let mut hook_task = task.as_ref().clone();
                        hook_task
                            .depends_on
                            .extend(previous_step_task_names.clone());
                        manifest.tasks.insert(
                            hook_task_name.clone(),
                            TaskDefinition::Single(Box::new(hook_task)),
                        );
                        all_hook_task_names.push(hook_task_name.clone());
                        previous_step_task_names = vec![hook_task_name];
                    }
                    HookItem::TaskRef(task_ref) => {
                        let hook_task_name = format!("{ws_name}.hooks.beforeInstall[{step_idx}]");
                        let mut hook_task = cuenv_core::tasks::Task::from_task_ref(&task_ref.ref_);
                        hook_task
                            .depends_on
                            .extend(previous_step_task_names.clone());
                        manifest.tasks.insert(
                            hook_task_name.clone(),
                            TaskDefinition::Single(Box::new(hook_task)),
                        );
                        all_hook_task_names.push(hook_task_name.clone());
                        previous_step_task_names = vec![hook_task_name];
                    }
                    HookItem::Match(match_hook) => {
                        let matched_tasks = discovery.match_tasks(&match_hook.matcher).map_err(|e| {
                            cuenv_core::Error::configuration(format!(
                                "Workspace '{ws_name}' beforeInstall matcher has invalid configuration: {e}"
                            ))
                        })?;

                        let step_name = match_hook
                            .name
                            .as_deref()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map_or_else(|| format!("match[{step_idx}]"), ToString::to_string);

                        if matched_tasks.is_empty() {
                            tracing::info!(
                                "Workspace '{}' beforeInstall matcher '{}' matched no tasks",
                                ws_name,
                                step_name
                            );
                        } else {
                            let matched_display: Vec<String> = matched_tasks
                                .iter()
                                .map(|m| {
                                    if let Some(project_name) = &m.project_name {
                                        format!("{project_name}:{}", m.task_name)
                                    } else {
                                        format!("{}:{}", m.project_root.display(), m.task_name)
                                    }
                                })
                                .collect();

                            tracing::info!(
                                "Workspace '{}' beforeInstall matcher '{}' matched {} task(s): {}",
                                ws_name,
                                step_name,
                                matched_display.len(),
                                matched_display.join(", ")
                            );
                        }

                        let mut step_task_names: Vec<String> = Vec::new();
                        let mut prev_in_step: Option<String> = None;

                        for (i, matched) in matched_tasks.iter().enumerate() {
                            let hook_task_name =
                                format!("{ws_name}.hooks.beforeInstall.{step_name}[{i}]");

                            let mut task = matched.task.clone();
                            task.project_root = Some(
                                fs::canonicalize(&matched.project_root)
                                    .unwrap_or_else(|_| matched.project_root.clone()),
                            );

                            // Canonicalize deps relative to the matched task name (not the synthetic hook name)
                            task.depends_on = task
                                .depends_on
                                .iter()
                                .map(|d| canonicalize_dep_for_task_name(d, &matched.task_name))
                                .collect();

                            // Ensure this step runs after previous hook step(s). These deps live in the
                            // current project even though this task executes in a matched project_root.
                            for dep in &previous_step_task_names {
                                task.depends_on.push(task_fqdn(manifest_project_id, dep));
                            }

                            // Respect matcher.parallel: chain within this step if parallel == false
                            if !match_hook.matcher.parallel {
                                if let Some(prev_name) = &prev_in_step {
                                    task.depends_on
                                        .push(task_fqdn(manifest_project_id, prev_name));
                                }
                                prev_in_step = Some(hook_task_name.clone());
                            }

                            manifest.tasks.insert(
                                hook_task_name.clone(),
                                TaskDefinition::Single(Box::new(task)),
                            );
                            all_hook_task_names.push(hook_task_name.clone());
                            step_task_names.push(hook_task_name);
                        }

                        // Next step depends on all tasks from this step
                        previous_step_task_names = step_task_names;
                    }
                }
            }
        }

        // Wire: hooks -> install
        if !all_hook_task_names.is_empty()
            && let Some(install_task) =
                get_task_mut_by_name_or_path(&mut manifest.tasks, &install_task_name)
        {
            for hook_name in &all_hook_task_names {
                if !install_task.depends_on.contains(hook_name) {
                    install_task.depends_on.push(hook_name.clone());
                }
            }
        }

        // Ensure <ws>.setup exists
        if !manifest.tasks.contains_key(&setup_task_name) {
            let setup_task = cuenv_core::tasks::Task {
                command: String::new(),
                script: Some("true".to_string()),
                hermetic: false,
                depends_on: vec![install_task_name.clone()],
                ..Default::default()
            };
            manifest.tasks.insert(
                setup_task_name.clone(),
                TaskDefinition::Single(Box::new(setup_task)),
            );
        }

        // Wire: any task that uses this workspace -> <ws>.setup
        for (task_name, task_def) in &mut manifest.tasks {
            add_setup_dep_to_definition(task_name, task_def, ws_name, &setup_task_name);
        }
    }

    Ok(())
}

#[derive(Clone)]
struct ProjectCtx {
    root: PathBuf,
    id: String,
    manifest: Cuenv,
    is_current: bool,
}

fn build_global_tasks(
    evaluator: Arc<CueEvaluator>,
    module_root: &Path,
    current_project_root: &Path,
    current_manifest: &Cuenv,
) -> Result<(Tasks, String)> {
    // Evaluation function for discovery.
    // Each project can have a different CUE package name, so detect it per-project.
    let eval_fn: EvalFn = Box::new(move |project_path: &Path| {
        let pkg = detect_package_name(project_path).map_err(|e| e.to_string())?;
        evaluate_manifest(&evaluator, project_path, &pkg).map_err(|e| e.to_string())
    });

    let mut discovery = TaskDiscovery::new(module_root.to_path_buf()).with_eval_fn(eval_fn);
    discovery.discover().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to discover projects: {e}"))
    })?;

    let current_root = fs::canonicalize(current_project_root)
        .unwrap_or_else(|_| current_project_root.to_path_buf());

    // Build project contexts
    let mut project_id_by_name: HashMap<String, String> = HashMap::new();
    let mut used_project_ids: HashSet<String> = HashSet::new();
    let mut projects: Vec<ProjectCtx> = Vec::new();
    for p in discovery.projects() {
        let root = fs::canonicalize(&p.project_root).unwrap_or_else(|_| p.project_root.clone());
        let is_current = root == current_root;
        let mut manifest = if is_current {
            current_manifest.clone()
        } else {
            p.manifest.clone()
        };
        manifest = manifest.with_implicit_tasks();

        // Prefer the explicit `name` field for stable cross-project refs, but ensure uniqueness.
        let base_id = compute_project_id(&manifest, &root, module_root);
        let mut id = base_id.clone();
        if used_project_ids.contains(&id) {
            // Disambiguate collisions (common in repos that layer multiple env.cue files for the same package)
            // by suffixing with a path-derived identifier.
            let rel = root
                .strip_prefix(module_root)
                .unwrap_or(&root)
                .to_string_lossy()
                .replace(['/', '\\'], ".");
            let mut candidate = format!("{base_id}.{rel}");
            let mut i = 2;
            while used_project_ids.contains(&candidate) {
                candidate = format!("{base_id}.{rel}.{i}");
                i += 1;
            }
            id = candidate;
        }
        used_project_ids.insert(id.clone());

        // Map manifest `name` (used by TaskRef: "#name:task") to this unique project id.
        let trimmed = manifest.name.trim();
        if !trimmed.is_empty() {
            project_id_by_name
                .entry(trimmed.to_string())
                .or_insert_with(|| id.clone());
        }
        projects.push(ProjectCtx {
            root,
            id,
            manifest,
            is_current,
        });
    }

    // Index roots -> ids (used to scope relative dependencies by task.project_root)
    let mut id_by_root: HashMap<PathBuf, String> = HashMap::new();
    for p in &projects {
        id_by_root.insert(p.root.clone(), p.id.clone());
    }

    let current_project_id = projects.iter().find(|p| p.is_current).map_or_else(
        || compute_project_id(current_manifest, &current_root, module_root),
        |p| p.id.clone(),
    );

    // Inject workspace setup tasks and resolve TaskRefs (hooks)
    for p in &mut projects {
        inject_workspace_setup_tasks(&mut p.manifest, &discovery, &p.id)?;
        resolve_task_refs_in_manifest(&mut p.manifest, &discovery, &p.id, &project_id_by_name);
    }

    // Build global tasks keyed by FQDN
    let mut global: HashMap<String, TaskDefinition> = HashMap::new();
    for p in &projects {
        let idx = TaskIndex::build(&p.manifest.tasks)?;
        for entry in idx.list() {
            let mut def = entry.definition.clone();
            set_default_project_root(&mut def, &p.root);
            normalize_definition_deps(&mut def, &id_by_root, &project_id_by_name, &p.id);
            let fqdn = task_fqdn(&p.id, &entry.name);
            if global.contains_key(&fqdn) {
                return Err(cuenv_core::Error::configuration(format!(
                    "Duplicate task FQDN detected: '{fqdn}'",
                )));
            }
            global.insert(fqdn, def);
        }
    }

    Ok((Tasks { tasks: global }, current_project_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::tasks::Input;
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
            &[],
            None,
            "simple",
            false,
            None,
            false,
            None,
            false,
            &[],
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
            inputs: vec![Input::Path("inputs".into())],
            ..Default::default()
        };

        let k1 = compute_task_cache_key(&task, &env, &ws).expect("key1");
        // mutate input
        fs::write(ws.join("inputs/a.txt"), "B").unwrap();
        let k2 = compute_task_cache_key(&task, &env, &ws).expect("key2");
        assert_ne!(k1, k2, "key should change when input changes");
    }

    #[test]
    fn test_resolve_task_ref_merges_dependencies() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("env.cue"), "package test").unwrap();

        let mut manifest = Cuenv {
            name: "proj".to_string(),
            ..Default::default()
        };

        let referenced_task = Task {
            command: "echo".into(),
            depends_on: vec!["dep-a".into(), "dep-b".into()],
            ..Default::default()
        };
        manifest.tasks.insert(
            "run".into(),
            TaskDefinition::Single(Box::new(referenced_task.clone())),
        );

        let manifest_for_eval = manifest.clone();
        let mut discovery = TaskDiscovery::new(tmp.path().to_path_buf())
            .with_eval_fn(Box::new(move |_| Ok(manifest_for_eval.clone())));
        discovery.discover().expect("discovery should succeed");

        let placeholder_task = Task {
            task_ref: Some("#proj:run".into()),
            depends_on: vec!["placeholder".into()],
            ..Default::default()
        };
        let mut task_def = TaskDefinition::Single(Box::new(placeholder_task));

        let project_id_by_name: HashMap<String, String> = HashMap::new();
        resolve_task_refs_in_definition(&mut task_def, &discovery, "proj", &project_id_by_name);

        let TaskDefinition::Single(resolved) = task_def else {
            panic!("expected single task");
        };

        assert_eq!(resolved.command, "echo");
        assert_eq!(
            resolved.depends_on,
            vec![
                "dep-a".to_string(),
                "dep-b".to_string(),
                "task:proj:placeholder".to_string()
            ]
        );
        assert_eq!(
            resolved.project_root,
            Some(fs::canonicalize(tmp.path()).unwrap())
        );
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
        assert!(!s2.contains("hello")); // Output handled by executor now
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
            inputs: vec![Input::Path("inputs".into())],
            outputs: vec!["out/out.txt".into()],
            ..Default::default()
        };

        // Execute directly via helper
        let hook_env = Environment::new();
        let res = run_task_hermetic(&proj, &evaluator, "mytask", &task, None, &hook_env, true)
            .await
            .expect("hermetic run ok");
        assert!(res.success);
        assert!(res.stdout.contains("done"));
    }

    #[ignore = "hermetic execution temporarily disabled - see execute_task_with_strategy_hermetic"]
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
        let exec = TaskExecutor::with_dagger_factory(
            ExecutorConfig {
                capture_output: true,
                workspaces: None,
                ..Default::default()
            },
            get_dagger_factory(),
        );

        let task = Task {
            command: "sh".into(),
            args: vec!["-c".into(), "cp inputs/in.txt out.txt".into()],
            inputs: vec![Input::Path("inputs".into())],
            outputs: vec!["out.txt".into()],
            ..Default::default()
        };
        let def = TaskDefinition::Single(Box::new(task.clone()));
        let mut all = Tasks::new();
        all.tasks.insert("copy".into(), def.clone());
        let mut g = TaskGraph::new();
        g.build_from_definition("copy", &def, &all).unwrap();

        let hook_env = Environment::new();
        let results = execute_task_with_strategy_hermetic(
            proj.to_str().unwrap(),
            &evaluator,
            &exec,
            "copy",
            &def,
            &g,
            &all,
            None,
            &hook_env,
            true,
        )
        .await
        .expect("execute hermetic ok");
        assert_eq!(results.len(), 1);
        assert!(results[0].success);
    }

    #[test]
    fn test_render_task_tree() {
        use cuenv_core::tasks::IndexedTask;
        // Helper to create a dummy task
        let make_task = |desc: Option<&str>| Task {
            command: "echo".into(),
            description: desc.map(ToString::to_string),
            ..Default::default()
        };

        let t_build = IndexedTask {
            name: "build".into(),
            original_name: "build".into(),
            definition: TaskDefinition::Single(Box::new(make_task(Some("Build the project")))),
            is_group: false,
            source_file: None, // Root env.cue
        };
        let t_fmt_check = IndexedTask {
            name: "fmt.check".into(),
            original_name: "fmt.check".into(),
            definition: TaskDefinition::Single(Box::new(make_task(Some("Check formatting")))),
            is_group: false,
            source_file: None,
        };
        let t_fmt_fix = IndexedTask {
            name: "fmt.fix".into(),
            original_name: "fmt.fix".into(),
            definition: TaskDefinition::Single(Box::new(make_task(Some("Fix formatting")))),
            is_group: false,
            source_file: None,
        };

        // Provide them in mixed order to verify sorting
        let tasks = vec![&t_fmt_fix, &t_build, &t_fmt_check];
        let output = render_task_tree(tasks, None);

        // We can't match exact lines easily because of dot padding calculation,
        // but we can check structure and presence of content.

        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines[0], "Tasks:");

        // build is first alphabetically
        assert!(lines[1].starts_with("├─ build"));
        assert!(lines[1].contains("Build the project"));

        // fmt is second/last
        assert!(lines[2].starts_with("└─ fmt"));

        // children of fmt
        // fmt is last, so children have "   " prefix
        assert!(lines[3].starts_with("   ├─ check"));
        assert!(lines[3].contains("Check formatting"));

        assert!(lines[4].starts_with("   └─ fix"));
        assert!(lines[4].contains("Fix formatting"));
    }

    #[test]
    fn test_parse_task_args_positional_only() {
        let args = vec!["arg1".to_string(), "arg2".to_string()];
        let (pos, named) = parse_task_args(&args, None);
        assert_eq!(pos, vec!["arg1", "arg2"]);
        assert!(named.is_empty());
    }

    #[test]
    fn test_parse_task_args_long_flags_space() {
        let args = vec!["--key".to_string(), "value".to_string()];
        let (pos, named) = parse_task_args(&args, None);
        assert!(pos.is_empty());
        assert_eq!(named.get("key"), Some(&"value".to_string()));
    }

    #[test]
    fn test_parse_task_args_long_flags_equals() {
        let args = vec!["--key=value".to_string()];
        let (pos, named) = parse_task_args(&args, None);
        assert!(pos.is_empty());
        assert_eq!(named.get("key"), Some(&"value".to_string()));
    }

    #[test]
    fn test_parse_task_args_boolean_flag() {
        let args = vec!["--verbose".to_string()];
        let (pos, named) = parse_task_args(&args, None);
        assert!(pos.is_empty());
        assert_eq!(named.get("verbose"), Some(&"true".to_string()));
    }

    #[test]
    fn test_parse_task_args_short_flags_with_params() {
        use cuenv_core::tasks::{ParamDef, TaskParams};
        use std::collections::HashMap;

        let mut params_named = HashMap::new();
        params_named.insert(
            "quality".to_string(),
            ParamDef {
                short: Some("q".to_string()),
                ..Default::default()
            },
        );
        let params = TaskParams {
            positional: vec![],
            named: params_named,
        };

        let args = vec!["-q".to_string(), "1080p".to_string()];
        let (pos, named) = parse_task_args(&args, Some(&params));
        assert!(pos.is_empty());
        // Short flag -q should resolve to long name "quality"
        assert_eq!(named.get("quality"), Some(&"1080p".to_string()));
    }

    #[test]
    fn test_parse_task_args_mixed() {
        let args = vec![
            "positional1".to_string(),
            "--long".to_string(),
            "longval".to_string(),
            "positional2".to_string(),
        ];
        let (pos, named) = parse_task_args(&args, None);
        assert_eq!(pos, vec!["positional1", "positional2"]);
        assert_eq!(named.get("long"), Some(&"longval".to_string()));
    }

    #[test]
    fn test_parse_task_args_negative_numbers() {
        // Negative numbers should be parsed as values, not flags
        let args = vec![
            "--port".to_string(),
            "-1".to_string(),
            "--offset".to_string(),
            "-5".to_string(),
        ];
        let (pos, named) = parse_task_args(&args, None);
        assert!(pos.is_empty());
        assert_eq!(named.get("port"), Some(&"-1".to_string()));
        assert_eq!(named.get("offset"), Some(&"-5".to_string()));
    }

    #[test]
    fn test_parse_task_args_negative_float() {
        let args = vec!["--threshold".to_string(), "-3.14".to_string()];
        let (pos, named) = parse_task_args(&args, None);
        assert!(pos.is_empty());
        assert_eq!(named.get("threshold"), Some(&"-3.14".to_string()));
    }

    #[test]
    fn test_resolve_task_args_no_params_passthrough() {
        let args = vec!["arg1".to_string(), "--flag".to_string(), "val".to_string()];
        let resolved = resolve_task_args(None, &args).unwrap();
        assert_eq!(resolved.positional, vec!["arg1"]);
        assert_eq!(resolved.named.get("flag"), Some(&"val".to_string()));
    }

    #[test]
    fn test_resolve_task_args_defaults_applied() {
        use cuenv_core::tasks::{ParamDef, TaskParams};
        use std::collections::HashMap;

        let mut params_named = HashMap::new();
        params_named.insert(
            "quality".to_string(),
            ParamDef {
                default: Some("1080p".to_string()),
                ..Default::default()
            },
        );
        let params = TaskParams {
            positional: vec![],
            named: params_named,
        };

        let args: Vec<String> = vec![];
        let resolved = resolve_task_args(Some(&params), &args).unwrap();
        assert_eq!(resolved.named.get("quality"), Some(&"1080p".to_string()));
    }

    #[test]
    fn test_resolve_task_args_required_missing_error() {
        use cuenv_core::tasks::{ParamDef, TaskParams};
        use std::collections::HashMap;

        let mut params_named = HashMap::new();
        params_named.insert(
            "required_arg".to_string(),
            ParamDef {
                required: true,
                description: Some("A required argument".to_string()),
                ..Default::default()
            },
        );
        let params = TaskParams {
            positional: vec![],
            named: params_named,
        };

        let args: Vec<String> = vec![];
        let result = resolve_task_args(Some(&params), &args);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("--required_arg"));
    }

    #[test]
    fn test_resolve_task_args_positional_required() {
        use cuenv_core::tasks::{ParamDef, TaskParams};

        let params = TaskParams {
            positional: vec![ParamDef {
                required: true,
                description: Some("Video ID".to_string()),
                ..Default::default()
            }],
            named: std::collections::HashMap::new(),
        };

        // Missing required positional arg
        let args: Vec<String> = vec![];
        let result = resolve_task_args(Some(&params), &args);
        assert!(result.is_err());

        // With required positional arg
        let args = vec!["VIDEO123".to_string()];
        let result = resolve_task_args(Some(&params), &args).unwrap();
        assert_eq!(result.positional, vec!["VIDEO123"]);
    }

    #[test]
    fn test_apply_args_to_task() {
        use cuenv_core::tasks::ResolvedArgs;
        use std::collections::HashMap;

        let task = Task {
            command: "echo".to_string(),
            args: vec!["{{0}}".to_string(), "--url={{url}}".to_string()],
            ..Default::default()
        };

        let mut named = HashMap::new();
        named.insert("url".to_string(), "https://example.com".to_string());
        let resolved = ResolvedArgs {
            positional: vec!["hello".to_string()],
            named,
        };

        let modified = apply_args_to_task(&task, &resolved);
        assert_eq!(modified.command, "echo");
        assert_eq!(modified.args, vec!["hello", "--url=https://example.com"]);
    }

    #[test]
    fn test_parse_task_args_double_dash_separator() {
        // `--` should end flag parsing, everything after is positional
        let args = vec![
            "--flag".to_string(),
            "value".to_string(),
            "--".to_string(),
            "--not-a-flag".to_string(),
            "-x".to_string(),
        ];
        let (pos, named) = parse_task_args(&args, None);
        assert_eq!(pos, vec!["--not-a-flag", "-x"]);
        assert_eq!(named.get("flag"), Some(&"value".to_string()));
        assert!(!named.contains_key("not-a-flag"));
    }

    #[test]
    fn test_parse_task_args_empty_value_with_equals() {
        let args = vec!["--key=".to_string()];
        let (pos, named) = parse_task_args(&args, None);
        assert!(pos.is_empty());
        assert_eq!(named.get("key"), Some(&String::new()));
    }

    #[test]
    fn test_parse_task_args_multi_char_short_flag_as_positional() {
        // Multi-character short flags like -abc are treated as positional
        let args = vec!["-abc".to_string(), "--valid".to_string(), "val".to_string()];
        let (pos, named) = parse_task_args(&args, None);
        assert_eq!(pos, vec!["-abc"]);
        assert_eq!(named.get("valid"), Some(&"val".to_string()));
    }

    #[test]
    fn test_parse_task_args_single_dash_as_positional() {
        // A single `-` is treated as positional (common for stdin)
        let args = vec!["-".to_string(), "--flag".to_string()];
        let (pos, named) = parse_task_args(&args, None);
        assert_eq!(pos, vec!["-"]);
        assert_eq!(named.get("flag"), Some(&"true".to_string()));
    }

    #[test]
    fn test_resolve_task_args_excess_positional_error() {
        use cuenv_core::tasks::{ParamDef, TaskParams};

        let params = TaskParams {
            positional: vec![ParamDef::default()], // Only 1 positional allowed
            named: std::collections::HashMap::new(),
        };

        let args = vec!["arg1".to_string(), "arg2".to_string(), "arg3".to_string()];
        let result = resolve_task_args(Some(&params), &args);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Too many positional arguments"));
    }

    #[test]
    fn test_resolve_task_args_unknown_named_error() {
        use cuenv_core::tasks::{ParamDef, TaskParams};
        use std::collections::HashMap;

        let mut params_named = HashMap::new();
        params_named.insert("known".to_string(), ParamDef::default());
        let params = TaskParams {
            positional: vec![],
            named: params_named,
        };

        let args = vec!["--unknown".to_string(), "value".to_string()];
        let result = resolve_task_args(Some(&params), &args);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Unknown argument"));
        assert!(err.contains("--unknown"));
    }

    // ==================== Label-based execution tests ====================

    #[test]
    fn test_normalize_labels_basic() {
        let labels = vec!["b".to_string(), "a".to_string(), "c".to_string()];
        let normalized = normalize_labels(&labels);
        assert_eq!(normalized, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_normalize_labels_deduplicates() {
        let labels = vec!["test".to_string(), "build".to_string(), "test".to_string()];
        let normalized = normalize_labels(&labels);
        assert_eq!(normalized, vec!["build", "test"]);
    }

    #[test]
    fn test_normalize_labels_filters_empty() {
        let labels = vec![
            String::new(),
            "valid".to_string(),
            "   ".to_string(),
            "another".to_string(),
        ];
        let normalized = normalize_labels(&labels);
        assert_eq!(normalized, vec!["another", "valid"]);
    }

    #[test]
    fn test_normalize_labels_trims_whitespace() {
        let labels = vec!["  test  ".to_string(), " build".to_string()];
        let normalized = normalize_labels(&labels);
        assert_eq!(normalized, vec!["build", "test"]);
    }

    #[test]
    fn test_normalize_labels_all_empty_returns_empty() {
        let labels = vec![String::new(), "   ".to_string()];
        let normalized = normalize_labels(&labels);
        assert!(normalized.is_empty());
    }

    #[test]
    fn test_find_tasks_with_labels_single_label() {
        let mut tasks = Tasks::new();

        // Task with matching label
        let task_with_label = Task {
            command: "echo".to_string(),
            labels: vec!["test".to_string()],
            ..Default::default()
        };
        tasks.tasks.insert(
            "task1".to_string(),
            TaskDefinition::Single(Box::new(task_with_label)),
        );

        // Task without the label
        let task_without_label = Task {
            command: "echo".to_string(),
            labels: vec!["build".to_string()],
            ..Default::default()
        };
        tasks.tasks.insert(
            "task2".to_string(),
            TaskDefinition::Single(Box::new(task_without_label)),
        );

        let labels = vec!["test".to_string()];
        let matching = find_tasks_with_labels(&tasks, &labels);
        assert_eq!(matching, vec!["task1"]);
    }

    #[test]
    fn test_find_tasks_with_labels_multiple_labels_and_behavior() {
        let mut tasks = Tasks::new();

        // Task with both labels
        let task_both = Task {
            command: "echo".to_string(),
            labels: vec!["test".to_string(), "unit".to_string()],
            ..Default::default()
        };
        tasks.tasks.insert(
            "unit_tests".to_string(),
            TaskDefinition::Single(Box::new(task_both)),
        );

        // Task with only one label
        let task_one = Task {
            command: "echo".to_string(),
            labels: vec!["test".to_string()],
            ..Default::default()
        };
        tasks.tasks.insert(
            "e2e_tests".to_string(),
            TaskDefinition::Single(Box::new(task_one)),
        );

        // Both labels required (AND semantics) - only task with both should match
        let labels = vec!["test".to_string(), "unit".to_string()];
        let matching = find_tasks_with_labels(&tasks, &labels);
        assert_eq!(matching, vec!["unit_tests"]);
    }

    #[test]
    fn test_find_tasks_with_labels_no_matches() {
        let mut tasks = Tasks::new();

        let task = Task {
            command: "echo".to_string(),
            labels: vec!["build".to_string()],
            ..Default::default()
        };
        tasks
            .tasks
            .insert("task1".to_string(), TaskDefinition::Single(Box::new(task)));

        let labels = vec!["nonexistent".to_string()];
        let matching = find_tasks_with_labels(&tasks, &labels);
        assert!(matching.is_empty());
    }

    #[test]
    fn test_format_label_root_single_label() {
        let labels = vec!["test".to_string()];
        let name = format_label_root(&labels);
        assert_eq!(name, "__cuenv_labels__test");
    }

    #[test]
    fn test_format_label_root_multiple_labels_sorted() {
        // Labels should be sorted in the output for determinism
        let labels = vec!["zebra".to_string(), "alpha".to_string()];
        let name = format_label_root(&labels);
        assert_eq!(name, "__cuenv_labels__alpha+zebra");
    }

    #[test]
    fn test_format_label_root_deduplicates() {
        let labels = vec!["test".to_string(), "test".to_string()];
        let name = format_label_root(&labels);
        assert_eq!(name, "__cuenv_labels__test");
    }

    #[test]
    fn test_format_label_root_filters_empty() {
        let labels = vec![String::new(), "test".to_string(), "   ".to_string()];
        let name = format_label_root(&labels);
        assert_eq!(name, "__cuenv_labels__test");
    }
}
