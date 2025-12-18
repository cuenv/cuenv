//! Task execution command implementation

use cuengine::ModuleEvalOptions;
use cuenv_core::ModuleEvaluation;
use cuenv_core::Result;
use cuenv_core::environment::Environment;
use cuenv_core::manifest::Project;
use cuenv_core::manifest::TaskRef;
use cuenv_core::tasks::discovery::{EvalFn, TaskDiscovery};
use cuenv_core::tasks::executor::{TASK_FAILURE_SNIPPET_LINES, summarize_task_failure};
use cuenv_core::tasks::{
    BackendFactory, ExecutorConfig, ResolvedArgs, Task, TaskDefinition, TaskExecutor, TaskGraph,
    TaskIndex, TaskParams, Tasks, WorkspaceTask,
};

use super::env_file::find_cue_module_root;
use super::{CommandExecutor, convert_engine_error, relative_path_from_root};
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
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};

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
///
/// When an `executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
#[allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools
)]
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
    interactive: bool,
    help: bool,
    all: bool,
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
    let manifest: Project =
        evaluate_manifest(Path::new(path), package, executor)?.with_implicit_tasks();
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
            let eval_fn: EvalFn = Box::new(move |p: &Path| {
                evaluate_manifest(p, &pkg, None).map_err(|e| e.to_string())
            });

            discovery = discovery.with_eval_fn(eval_fn);
            if let Err(e) = discovery.discover() {
                tracing::warn!("Workspace discovery had errors: {}", e);
            }
        }

        let workspace_tasks = collect_workspace_tasks(&discovery);

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
                // Recursively call execute_task with the selected task
                return Box::pin(execute_task(
                    path,
                    package,
                    Some(&selected_task),
                    labels,
                    environment,
                    format,
                    capture_output,
                    materialize_outputs,
                    show_cache_path,
                    backend,
                    tui,
                    false, // interactive = false for the actual execution
                    help,
                    all,
                    task_args,
                    executor,
                ))
                .await;
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
        let task_data = build_task_list(&tasks, cwd_relative.as_deref());

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
        let requested_task = task_name.expect("task_name required when no labels provided");
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
            (tasks.clone(), display_task_name.clone())
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
            .expect("synthetic task missing after insertion");
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
            tracing::warn!("TUI error (task execution may have succeeded): {e}");
            eprintln!("Warning: TUI encountered an error: {e}");
            eprintln!("Task output may not have been fully displayed. Check logs for details.");
        }
        Err(e) => {
            // TUI task panicked or was cancelled
            tracing::error!("TUI task failed: {e}");
            eprintln!("Warning: TUI terminated unexpectedly: {e}");
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

/// Evaluate a CUE manifest using module-wide evaluation.
///
/// This function evaluates the entire CUE module once and extracts the Project
/// configuration at the specified directory. It provides helpful error messages
/// when the config uses Base schema instead of Project (tasks require Project).
///
/// When an `executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
fn evaluate_manifest(
    dir: &Path,
    package: &str,
    executor: Option<&CommandExecutor>,
) -> Result<Project> {
    let target_path = dir.canonicalize().map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(dir.to_path_buf().into_boxed_path()),
        operation: "canonicalize path".to_string(),
    })?;

    // Use executor's cached module if available
    if let Some(exec) = executor {
        tracing::debug!("Using cached module evaluation from executor");
        let module = exec.get_module(&target_path)?;
        let rel_path = relative_path_from_root(&module.root, &target_path);

        let instance = module.get(&rel_path).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "No CUE instance found at path: {} (relative: {})",
                target_path.display(),
                rel_path.display()
            ))
        })?;

        // Check if this is a Project (has name field) or Base (no name)
        return match instance.kind {
            cuenv_core::InstanceKind::Project => instance.deserialize(),
            cuenv_core::InstanceKind::Base => {
                // Valid Base config, but this command needs Project
                Err(cuenv_core::Error::configuration(
                    "This directory uses schema.#Base which doesn't support tasks.\n\
                     To use tasks, update your env.cue to use schema.#Project:\n\n\
                     schema.#Project\n\
                     name: \"your-project-name\"",
                ))
            }
        };
    }

    // Legacy path: fresh evaluation
    tracing::debug!("Using fresh module evaluation (no executor)");

    let module_root = find_cue_module_root(&target_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE module found (looking for cue.mod/) starting from: {}",
            target_path.display()
        ))
    })?;

    let options = ModuleEvalOptions {
        recursive: true,
        ..Default::default()
    };
    let raw_result = cuengine::evaluate_module(&module_root, package, Some(options))
        .map_err(convert_engine_error)?;

    let module = ModuleEvaluation::from_raw(
        module_root.clone(),
        raw_result.instances,
        raw_result.projects,
    );

    let rel_path = relative_path_from_root(&module_root, &target_path);
    let instance = module.get(&rel_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE instance found at path: {} (relative: {})",
            target_path.display(),
            rel_path.display()
        ))
    })?;

    // Check if this is a Project (has name field) or Base (no name)
    match instance.kind {
        cuenv_core::InstanceKind::Project => instance.deserialize(),
        cuenv_core::InstanceKind::Base => {
            // Valid Base config, but this command needs Project
            Err(cuenv_core::Error::configuration(
                "This directory uses schema.#Base which doesn't support tasks.\n\
                 To use tasks, update your env.cue to use schema.#Project:\n\n\
                 schema.#Project\n\
                 name: \"your-project-name\"",
            ))
        }
    }
}

fn get_task_cli_help() -> String {
    use clap::CommandFactory;
    let mut cmd = crate::cli::Cli::command();
    // Navigate to the "task" subcommand
    for subcmd in cmd.get_subcommands_mut() {
        if subcmd.get_name() == "task" {
            return subcmd.render_help().to_string();
        }
    }
    // Fallback (shouldn't happen)
    "Execute a task defined in CUE configuration\n\nUsage: cuenv task [OPTIONS] [NAME]".to_string()
}

fn format_task_detail(task: &cuenv_core::tasks::IndexedTask) -> String {
    let mut output = String::new();
    writeln!(output, "Task: {}", task.name).expect("write to string");

    match &task.definition {
        TaskDefinition::Single(t) => {
            if let Some(desc) = &t.description {
                writeln!(output, "Description: {desc}").expect("write to string");
            }
            writeln!(output, "Command: {}", t.command).expect("write to string");
            if !t.args.is_empty() {
                writeln!(output, "Args: {:?}", t.args).expect("write to string");
            }
            if !t.depends_on.is_empty() {
                writeln!(output, "Depends on: {:?}", t.depends_on).expect("write to string");
            }
            if !t.inputs.is_empty() {
                writeln!(output, "Inputs: {:?}", t.inputs).expect("write to string");
            }
            if !t.outputs.is_empty() {
                writeln!(output, "Outputs: {:?}", t.outputs).expect("write to string");
            }
            // Show params if defined
            if let Some(params) = &t.params {
                if !params.positional.is_empty() {
                    writeln!(output, "\nPositional Arguments:").expect("write to string");
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
                        writeln!(output, "  {{{{{i}}}}}{required}{default}{desc}")
                            .expect("write to string");
                    }
                }
                if !params.named.is_empty() {
                    writeln!(output, "\nNamed Arguments:").expect("write to string");
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
                        writeln!(output, "  {short}--{name}{required}{default}{desc}")
                            .expect("write to string");
                    }
                }
            }
        }
        TaskDefinition::Group(g) => {
            writeln!(output, "Type: Task Group").expect("write to string");
            match g {
                cuenv_core::tasks::TaskGroup::Sequential(_) => {
                    writeln!(output, "Mode: Sequential").expect("write to string");
                }
                cuenv_core::tasks::TaskGroup::Parallel(_) => {
                    writeln!(output, "Mode: Parallel").expect("write to string");
                }
            }
        }
    }
    output
}

/// Collect all tasks from discovered projects into `WorkspaceTask` format
fn collect_workspace_tasks(discovery: &TaskDiscovery) -> Vec<WorkspaceTask> {
    let mut result = Vec::new();

    for project in discovery.projects() {
        let project_name = project.manifest.name.trim();
        if project_name.is_empty() {
            continue;
        }

        if let Ok(index) = TaskIndex::build(&project.manifest.tasks) {
            for entry in index.list() {
                // Get description from task definition (only available for single tasks)
                let description = entry.definition.as_single().and_then(|t| {
                    let desc = t.description();
                    if desc.is_empty() {
                        None
                    } else {
                        Some(desc.to_string())
                    }
                });

                result.push(WorkspaceTask {
                    project: project_name.to_string(),
                    task: entry.name.clone(),
                    task_ref: format!("#{}:{}", project_name, entry.name),
                    description,
                    is_group: entry.is_group,
                });
            }
        }
    }

    result
}

/// Render workspace tasks in human-readable format
fn render_workspace_task_list(tasks: &[WorkspaceTask]) -> String {
    if tasks.is_empty() {
        return "No tasks found in workspace".to_string();
    }

    let mut output = String::new();
    let mut by_project: BTreeMap<&str, Vec<&WorkspaceTask>> = BTreeMap::new();

    for task in tasks {
        by_project.entry(&task.project).or_default().push(task);
    }

    for (project, project_tasks) in by_project {
        writeln!(output, "\n{project}").expect("write to string");
        writeln!(output, "{}", "─".repeat(project.len())).expect("write to string");

        for task in project_tasks {
            let desc = task
                .description
                .as_ref()
                .map(|d| format!(" - {d}"))
                .unwrap_or_default();
            writeln!(output, "  {}{}", task.task_ref, desc).expect("write to string");
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
        writeln!(output, "{header}").expect("write to string");

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

        write!(output, "{prefix}{marker}{name}").expect("write to string");

        if let Some(desc) = &node.description {
            // Pad with dots
            let padding = max_width.saturating_sub(current_line_len);
            // Add a minimum spacing
            let dots = ".".repeat(padding + 4);
            write!(output, " {dots} {desc}").expect("write to string");
        }
        writeln!(output).expect("write to string");

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

fn compute_project_id(manifest: &Project, project_root: &Path, module_root: &Path) -> String {
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
    manifest: &mut Project,
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
    manifest: &mut Project,
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
    manifest: Project,
    is_current: bool,
}

#[allow(clippy::too_many_lines)]
fn build_global_tasks(
    module_root: &Path,
    current_project_root: &Path,
    current_manifest: &Project,
    executor: Option<&CommandExecutor>,
) -> Result<(Tasks, String)> {
    let mut discovery = TaskDiscovery::new(module_root.to_path_buf());

    // Use executor's cached module if available (single evaluation for all projects).
    // All projects must use `package cuenv` - this is enforced by the CUE schema.
    if let Some(exec) = executor {
        tracing::debug!("Using cached module for global task registry build");
        let module = exec.get_module(module_root)?;

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
                        "Failed to deserialize project for global task registry"
                    );
                }
            }
        }
    } else {
        // Legacy path: use EvalFn for per-project evaluation (when no executor available)
        tracing::debug!("Using legacy EvalFn for global task registry build");
        let eval_fn: EvalFn = Box::new(move |project_path: &Path| {
            evaluate_manifest(project_path, "cuenv", None).map_err(|e| e.to_string())
        });

        discovery = discovery.with_eval_fn(eval_fn);
        discovery.discover().map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to discover projects: {e}"))
        })?;
    }

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
        let temp_dir = TempDir::new().expect("write to string");
        let cue_content = r#"package test
env: {
    FOO: "bar"
}"#;
        fs::write(temp_dir.path().join("env.cue"), cue_content).expect("write to string");

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
            false, // tui
            false, // interactive
            false, // help
            false, // all
            &[],
            None, // executor
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
        let resolved = resolve_task_args(None, &args).expect("write to string");
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
        let resolved = resolve_task_args(Some(&params), &args).expect("write to string");
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
        let result = resolve_task_args(Some(&params), &args).expect("write to string");
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
