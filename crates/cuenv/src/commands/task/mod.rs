//! Task execution command implementation

mod arguments;
mod discovery;
pub mod list_builder;
pub mod normalization;
mod rendering;
mod resolution;
mod types;
mod workspace;

// Re-export types for the public API. Some types may not be used externally yet.
#[allow(unused_imports)]
pub use types::{ExecutionMode, OutputConfig, TaskExecutionRequest, TaskSelection};

use arguments::{apply_args_to_task, resolve_task_args};
use discovery::{evaluate_manifest, find_tasks_with_labels, format_label_root, normalize_labels};
use list_builder::prepare_task_index;
use normalization::{compute_project_id, task_fqdn};
use rendering::{
    collect_workspace_tasks, format_task_detail, get_task_cli_help, render_task_tree,
    render_workspace_task_list,
};
use workspace::build_global_tasks;

use cuenv_core::Result;
use cuenv_core::environment::Environment;
use cuenv_core::manifest::Project;
use cuenv_core::tasks::discovery::{EvalFn, TaskDiscovery};
use cuenv_core::tasks::executor::{TASK_FAILURE_SNIPPET_LINES, summarize_task_failure};
use cuenv_core::tasks::{
    BackendFactory, ExecutorConfig, Task, TaskDefinition, TaskExecutor, TaskGraph, Tasks,
};

use super::CommandExecutor;
use super::env_file::find_cue_module_root;
use crate::tui::rich::RichTui;
use crate::tui::state::TaskInfo;

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
use std::fmt::Write;
use std::path::Path;

use super::export::get_environment_with_hooks;

/// Execute a task using the new structured request API.
///
/// This is the preferred entry point for task execution. It accepts a
/// `TaskExecutionRequest` which groups all parameters into a structured
/// format with type-safe selection modes.
///
/// # Errors
///
/// Returns an error if task resolution, validation, or execution fails.
///
/// # Example
///
/// ```ignore
/// let request = TaskExecutionRequest::named("./", "cuenv", "build")
///     .with_args(vec!["--release".to_string()])
///     .with_environment("prod");
///
/// let output = execute(request).await?;
/// ```
pub async fn execute(request: TaskExecutionRequest<'_>) -> Result<String> {
    // Extract parameters from the request and delegate to the legacy implementation
    let (task_name, labels, task_args, interactive, all) = match &request.selection {
        TaskSelection::Named { name, args } => {
            (Some(name.as_str()), &[][..], args.as_slice(), false, false)
        }
        TaskSelection::Labels(l) => (None, l.as_slice(), &[][..], false, false),
        TaskSelection::List => (None, &[][..], &[][..], false, false),
        TaskSelection::Interactive => (None, &[][..], &[][..], true, false),
        TaskSelection::All => (None, &[][..], &[][..], false, true),
    };

    let tui = request.execution_mode == ExecutionMode::Tui;

    execute_task_legacy(
        &request.path,
        &request.package,
        task_name,
        labels,
        request.environment.as_deref(),
        &request.output.format,
        request.output.capture_output,
        request
            .output
            .materialize_outputs
            .as_ref()
            .and_then(|p| p.to_str()),
        request.output.show_cache_path,
        request.backend.as_deref(),
        tui,
        interactive,
        request.output.help,
        all,
        request.skip_dependencies,
        task_args,
        request.executor,
    )
    .await
}

/// Internal implementation of task execution.
#[allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools
)]
async fn execute_task_legacy(
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
    interactive: bool,
    help: bool,
    all: bool,
    skip_dependencies: bool,
    task_args: &[String],
    executor: Option<&CommandExecutor>,
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

    // Evaluate CUE to get tasks and environment using module-wide evaluation
    let mut manifest: Project = evaluate_manifest(Path::new(path), package, executor)?;
    tracing::debug!("CUE evaluation successful");

    tracing::debug!(
        "Successfully parsed CUE evaluation, found {} tasks",
        manifest.tasks.len()
    );

    // We may need the cue.mod root later for global task discovery / cross-project deps.
    let project_root =
        std::fs::canonicalize(path).unwrap_or_else(|_| Path::new(path).to_path_buf());
    let cue_module_root = find_cue_module_root(&project_root);

    // Build TaskDiscovery for synthetic task injection (workspace hooks)
    // Use executor's cached module if available, otherwise skip discovery
    let discovery = if let Some(exec) = executor {
        if let Some(cue_mod_root) = cue_module_root.as_ref() {
            if let Ok(module) = exec.get_module(cue_mod_root) {
                let mut disc = TaskDiscovery::new(cue_mod_root.clone());
                for instance in module.projects() {
                    if let Ok(project) = instance.deserialize::<Project>() {
                        let proj_root = module.root.join(&instance.path);
                        disc.add_project(proj_root, project);
                    }
                }
                Some(disc)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // Compute project ID for workspace setup injection
    let project_id = cue_module_root
        .as_ref()
        .map(|root| compute_project_id(&manifest, &project_root, root))
        .unwrap_or_default();

    // Build a canonical index to support nested task paths (with synthetic tasks injected)
    let task_index = prepare_task_index(&mut manifest, discovery.as_ref(), &project_id)?;
    let local_tasks = task_index.to_tasks();

    // Handle workspace-wide task listing for IDE completions
    if all && task_name.is_none() && labels.is_empty() {
        tracing::debug!("Listing workspace-wide tasks for IDE completions");

        let Some(cue_mod_root) = cue_module_root.as_ref() else {
            return Err(cuenv_core::Error::configuration(
                "Cannot use --all outside of a CUE module (no cue.mod found)",
            ));
        };

        let mut discovery = TaskDiscovery::new(cue_mod_root.clone());

        // Use executor's cached module if available (single evaluation for all projects)
        if let Some(exec) = executor {
            tracing::debug!("Using cached module for workspace task discovery");
            let module = exec.get_module(cue_mod_root)?;

            // Iterate through all Project instances and add them directly
            for instance in module.projects() {
                match instance.deserialize::<Project>() {
                    Ok(project) => {
                        let project_root = module.root.join(&instance.path);
                        discovery.add_project(project_root, project);
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %instance.path.display(),
                            error = %e,
                            "Failed to deserialize project - tasks will not be available"
                        );
                    }
                }
            }
        } else {
            // Legacy path: use EvalFn for per-project evaluation
            tracing::debug!("Using legacy EvalFn for workspace task discovery");
            let pkg = package.to_string();
            let eval_fn: EvalFn = Box::new(move |p: &Path| evaluate_manifest(p, &pkg, None));

            discovery = discovery.with_eval_fn(eval_fn);
            if let Err(e) = discovery.discover() {
                tracing::warn!("Workspace discovery had errors: {}", e);
            }
        }

        let workspace_tasks = collect_workspace_tasks(&discovery, cue_mod_root);

        if format == "json" {
            return serde_json::to_string(&workspace_tasks).map_err(|e| {
                cuenv_core::Error::configuration(format!(
                    "Failed to serialize workspace tasks: {e}"
                ))
            });
        }

        // Human-readable format for workspace tasks
        return Ok(render_workspace_task_list(&workspace_tasks));
    }

    // Handle interactive mode: show picker and execute selected task
    if interactive && task_name.is_none() && labels.is_empty() {
        use super::task_picker::{PickerResult, SelectableTask, run_picker};

        let tasks = task_index.list();
        let selectable: Vec<SelectableTask> = tasks
            .iter()
            .filter(|t| {
                // Only show executable tasks, not groups
                matches!(
                    t.definition,
                    TaskDefinition::Single(_) | TaskDefinition::Group(_)
                )
            })
            .map(|t| {
                let description = match &t.definition {
                    TaskDefinition::Single(task) => task.description.clone(),
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
                SelectableTask {
                    name: t.name.clone(),
                    description,
                }
            })
            .collect();

        match run_picker(selectable) {
            Ok(PickerResult::Selected(selected_task)) => {
                // Build a new request for the selected task
                let mut request =
                    TaskExecutionRequest::named(path, package, &selected_task).with_format(format);

                if let Some(env) = environment {
                    request = request.with_environment(env);
                }
                if capture_output {
                    request = request.with_capture();
                }
                if let Some(mat_path) = materialize_outputs {
                    request = request.with_materialize_outputs(mat_path);
                }
                if show_cache_path {
                    request = request.with_show_cache_path();
                }
                if let Some(be) = backend {
                    request = request.with_backend(be);
                }
                if tui {
                    request = request.with_tui();
                }
                if help {
                    request = request.with_help();
                }
                if skip_dependencies {
                    request = request.with_skip_dependencies();
                }
                if let Some(exec) = executor {
                    request = request.with_executor(exec);
                }

                return Box::pin(execute(request)).await;
            }
            Ok(PickerResult::Cancelled) => {
                return Ok(String::new());
            }
            Err(e) => {
                return Err(cuenv_core::Error::configuration(format!(
                    "Interactive picker failed: {e}"
                )));
            }
        }
    }

    // If no task specified, list available tasks
    if task_name.is_none() && labels.is_empty() {
        use super::task_list::{RichFormatter, TaskListFormatter, TextFormatter, build_task_list};
        use std::io::IsTerminal;

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

        // Build task list data
        let task_data = build_task_list(&tasks, cwd_relative.as_deref(), &project_root);

        // Select formatter based on format flag and TTY detection
        let output = match format {
            "rich" => {
                let formatter = RichFormatter::new();
                formatter.format(&task_data)
            }
            "text" => {
                let formatter = TextFormatter;
                formatter.format(&task_data)
            }
            _ => {
                // Default: rich for TTY, text otherwise
                if std::io::stdout().is_terminal() {
                    let formatter = RichFormatter::new();
                    formatter.format(&task_data)
                } else {
                    let formatter = TextFormatter;
                    formatter.format(&task_data)
                }
            }
        };

        return Ok(output);
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
        let requested_task = task_name.ok_or_else(|| {
            cuenv_core::Error::configuration("task name required when no labels provided")
        })?;
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
                .insert(display_task_name.clone(), modified_def.clone());

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
                build_global_tasks(module_root, &project_root, &manifest, executor)?;

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
            (tasks, display_task_name.clone())
        };

        all_tasks = global_tasks;
        task_graph_root_name = task_root_name;
    } else {
        // Execute tasks by label
        let (mut tasks_in_scope, _current_project_id) = if let Some(module_root) = &cue_module_root
        {
            build_global_tasks(module_root, &project_root, &manifest, executor).map_err(|e| {
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
            .ok_or_else(|| {
                cuenv_core::Error::execution("synthetic task missing after insertion")
            })?;
        task_graph_root_name = display_task_name.clone();
        all_tasks = tasks_in_scope;
    }

    // Get environment with hook-generated vars merged in
    let directory = project_root.clone();
    let base_env_vars =
        get_environment_with_hooks(&directory, &manifest, package, executor).await?;

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
        let (task_env_vars, secrets) =
            cuenv_core::environment::Environment::resolve_for_task_with_secrets(
                display_task_name.as_str(),
                &env_vars,
            )
            .await?;

        // Register resolved secrets for global redaction in the events system.
        // This ensures they're redacted from ALL output, not just this task's output.
        cuenv_events::register_secrets(secrets.into_iter());

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
        cue_module_root: cue_module_root.clone(),
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

    if skip_dependencies {
        // When skipping dependencies, just add the target task without its dependency tree.
        // This is used by CI orchestrators (like GitHub Actions) that handle dependencies externally.
        tracing::debug!("Skipping dependencies - adding only the target task");
        if let Some(TaskDefinition::Single(task)) = all_tasks.get(&task_graph_root_name) {
            task_graph.add_task(&task_graph_root_name, (**task).clone())?;
        }
    } else {
        task_graph
            .build_for_task(&task_graph_root_name, &all_tasks)
            .map_err(|e| {
                tracing::error!("Failed to build task graph: {}", e);
                e
            })?;
    }

    tracing::debug!(
        "Successfully built task graph with {} tasks",
        task_graph.task_count()
    );

    // If TUI is requested and we have a task graph, launch the rich TUI
    if tui && task_graph.task_count() > 0 {
        // For TUI mode, we MUST capture output so it goes through the event system
        // rather than directly to stdout/stderr (which would corrupt the TUI display).
        let tui_config = ExecutorConfig {
            capture_output: true, // Force capture for TUI mode
            max_parallel: 0,
            environment: runtime_env.clone(),
            working_dir: None,
            cue_module_root: cue_module_root.clone(),
            project_root: project_root.clone(),
            materialize_outputs: materialize_outputs.map(|s| Path::new(s).to_path_buf()),
            cache_dir: None,
            show_cache_path,
            workspaces: manifest.workspaces.clone(),
            backend_config: manifest.config.as_ref().and_then(|c| c.backend.clone()),
            cli_backend: backend.map(ToString::to_string),
        };
        let tui_executor = TaskExecutor::with_dagger_factory(tui_config, get_dagger_factory());

        return execute_with_rich_tui(
            path,
            &tui_executor,
            display_task_name.as_str(),
            &task_def,
            &task_graph,
            &all_tasks,
            manifest.env.as_ref(),
            &runtime_env,
        )
        .await;
    }

    // Execute using the appropriate method
    let results = execute_task_with_strategy(
        &executor,
        display_task_name.as_str(),
        &task_def,
        &task_graph,
        &all_tasks,
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
///
/// Note: The executor MUST have `capture_output: true` to ensure task output
/// goes through the event system rather than directly to stdout/stderr.
#[allow(clippy::too_many_arguments)]
async fn execute_with_rich_tui(
    _project_dir: &str,
    executor: &TaskExecutor,
    task_name: &str,
    _task_def: &TaskDefinition,
    task_graph: &TaskGraph,
    _all_tasks: &Tasks,
    _env_base: Option<&cuenv_core::environment::Env>,
    _hook_env: &Environment,
) -> Result<String> {
    // Subscribe to the global event bus.
    // The global bus is set up during CLI initialization and receives all events
    // emitted via the emit_task_*! macros through the global tracing subscriber.
    let event_rx = crate::tracing::subscribe_global_events().ok_or_else(|| {
        cuenv_core::Error::configuration(
            "Global event bus not initialized - TUI requires event-based tracing".to_string(),
        )
    })?;

    // Create oneshot channel for TUI readiness signaling.
    // This prevents a race condition where task execution starts
    // before the TUI event loop is ready to receive events.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    // Create and initialize TUI
    let mut tui = RichTui::new(event_rx, ready_tx)
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to initialize TUI: {e}")))?;

    // Build TaskInfo structs from the task graph
    let mut task_infos = Vec::new();
    let sorted_tasks = task_graph
        .topological_sort()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to sort task graph: {e}")))?;

    // Calculate levels based on dependencies
    let mut levels: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for node in &sorted_tasks {
        let max_dep_level = node
            .task
            .depends_on
            .iter()
            .filter_map(|dep| levels.get(dep).copied())
            .max()
            .unwrap_or(0);
        let increment = usize::from(!node.task.depends_on.is_empty());
        levels.insert(node.name.clone(), max_dep_level.saturating_add(increment));
    }

    for node in sorted_tasks {
        let task_name = node.name.clone();
        let dependencies: Vec<String> = node.task.depends_on.clone();
        let level = levels.get(&task_name).copied().unwrap_or(0);

        task_infos.push(TaskInfo::new(task_name, dependencies, level));
    }

    tui.init_tasks(task_infos);

    // Run TUI and task execution concurrently
    // Note: TUI run() is blocking (uses crossterm::event::poll), so we spawn_blocking
    let tui_handle = tokio::task::spawn_blocking(move || tui.run());

    // Wait for TUI to signal it's ready before starting task execution.
    // This prevents a race condition where early events are missed.
    if ready_rx.await.is_err() {
        // TUI failed to start or was dropped before signaling ready
        return Err(cuenv_core::Error::configuration(
            "TUI failed to initialize - event loop did not start".to_string(),
        ));
    }

    // Execute tasks
    let results = executor.execute_graph(task_graph).await?;

    // Determine overall success
    let all_succeeded = results.iter().all(|r| r.success);

    // Emit completion event so the TUI knows execution is done.
    // This must happen BEFORE the event bus sender is dropped.
    cuenv_events::emit_command_completed!("task", all_succeeded, 0_u64);

    // Wait for TUI to finish and handle any errors.
    // Note: No sleep is needed here because:
    // 1. The TUI polls for events every 50ms
    // 2. We're waiting for the user to dismiss the TUI (via tui_handle.await)
    // 3. The channel stays open until this function returns (after TUI finishes)
    // Note: By this point, the TUI's TerminalGuard has been dropped,
    // so the terminal is restored and stderr output will be visible.
    match tui_handle.await {
        Ok(Ok(())) => {
            // TUI completed successfully
        }
        Ok(Err(e)) => {
            // TUI returned an error - log it but don't fail the task execution
            // since the tasks themselves may have succeeded
            tracing::warn!(error = %e, "TUI error (task execution may have succeeded)");
            cuenv_events::emit_stderr!(format!("Warning: TUI encountered an error: {e}"));
            cuenv_events::emit_stderr!(
                "Task output may not have been fully displayed. Check logs for details."
            );
        }
        Err(e) => {
            // TUI task panicked or was cancelled
            tracing::error!(error = %e, "TUI task failed");
            cuenv_events::emit_stderr!(format!("Warning: TUI terminated unexpectedly: {e}"));
        }
    }

    // Check for failures
    if let Some(failed) = results.iter().find(|r| !r.success) {
        return Err(cuenv_core::Error::configuration(summarize_task_failure(
            failed,
            TASK_FAILURE_SNIPPET_LINES,
        )));
    }

    // Return success message
    Ok(format!(
        "Task '{task_name}' completed successfully in TUI mode"
    ))
}

/// Execute a task using the appropriate strategy based on task type and dependencies.
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
            executor
                .execute_definition(task_name, task_def, all_tasks)
                .await
        }
        TaskDefinition::Single(_) => {
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

fn format_task_results(
    results: Vec<cuenv_core::tasks::TaskResult>,
    capture_output: bool,
    task_name: &str,
) -> String {
    let mut output = String::new();
    for result in results {
        if capture_output {
            write!(output, "Task '{}' ", result.name).expect("write to string");
            if result.success {
                output.push_str("succeeded\n");
                if !result.stdout.is_empty() {
                    output.push_str("Output:\n");
                    output.push_str(&result.stdout);
                    output.push('\n');
                }
            } else {
                writeln!(output, "failed with exit code {:?}", result.exit_code)
                    .expect("write to string");
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

#[cfg(test)]
mod tests {
    use super::*;
    use resolution::resolve_task_refs_in_definition;

    use std::collections::HashMap;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_list_tasks_empty() {
        let temp_dir = TempDir::new().expect("write to string");
        let cue_content = r#"package test
env: {
    FOO: "bar"
}"#;
        fs::write(temp_dir.path().join("env.cue"), cue_content).expect("write to string");

        let request = TaskExecutionRequest::list(temp_dir.path().to_str().unwrap(), "test");
        let result = execute(request).await;

        // The result depends on FFI availability
        if let Ok(output) = result {
            assert!(output.contains("No tasks") || output.contains("Available tasks"));
        } else {
            // FFI not available in test environment
        }
    }

    #[test]
    fn test_resolve_task_ref_merges_dependencies() {
        let tmp = TempDir::new().expect("write to string");
        fs::write(tmp.path().join("env.cue"), "package test").expect("write to string");

        let mut manifest = Project {
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
}
