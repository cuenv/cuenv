use super::dag_export;
use super::discovery::evaluate_manifest;
use super::list_builder::prepare_task_index;
use super::rendering::{format_task_detail, get_task_cli_help, render_task_tree};
use super::types::{ExecutionMode, OutputConfig, TaskExecutionRequest, TaskSelection};
use super::{
    build_task_cache, execute_task_with_strategy, execute_with_rich_tui, format_task_results,
    get_dagger_factory, resolve_runtime_cache_identity,
};
use crate::commands::env_file::find_cue_module_root;
use crate::commands::export::{HookEnvironmentRequest, get_environment_with_hooks};
use crate::commands::tools::{ensure_tools_downloaded, resolve_tool_activation_steps};
use cuenv_core::environment::Environment;
use cuenv_core::manifest::{Project, Runtime};
use cuenv_core::runtime::resolve_runtime_environment;
use cuenv_core::tasks::cache::TaskCacheConfig;
use cuenv_core::tasks::executor::{TASK_FAILURE_SNIPPET_LINES, summarize_task_failure};
use cuenv_core::tasks::{ExecutorConfig, TaskExecutor, TaskGraph, TaskIndex, TaskNode, Tasks};
use cuenv_core::tools::apply_resolved_tool_activation;
use cuenv_core::{DryRun, OutputCapture, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

mod listing;
mod picker;
mod selection;

use listing::maybe_render_task_list;
use picker::maybe_run_interactive_picker;
use selection::{TaskResolution, resolve_task_selection, validate_task_selection};

struct TaskExecutionInput<'a> {
    selection: &'a TaskSelection,
    output: &'a OutputConfig,
    execution_mode: &'a ExecutionMode,
    path: &'a str,
    package: &'a str,
    environment: Option<&'a str>,
    format: &'a str,
    capture_output: OutputCapture,
    materialize_outputs: Option<&'a str>,
    backend: Option<&'a str>,
    skip_dependencies: bool,
    continue_on_error: bool,
    dry_run: DryRun,
    executor: &'a crate::commands::CommandExecutor,
    task_name: Option<&'a str>,
    labels: &'a [String],
    task_args: &'a [String],
}

impl<'a> TaskExecutionInput<'a> {
    fn from_request(request: &'a TaskExecutionRequest<'_>) -> Self {
        let (task_name, labels, task_args) = match &request.selection {
            TaskSelection::Named { name, args } => (Some(name.as_str()), &[][..], args.as_slice()),
            TaskSelection::Labels(labels) => (None, labels.as_slice(), &[][..]),
            TaskSelection::List | TaskSelection::Interactive => (None, &[][..], &[][..]),
        };

        Self {
            selection: &request.selection,
            output: &request.output,
            execution_mode: &request.execution_mode,
            path: &request.path,
            package: &request.package,
            environment: request.environment.as_deref(),
            format: &request.output.format,
            capture_output: request.output.capture_output,
            materialize_outputs: request
                .output
                .materialize_outputs
                .as_ref()
                .and_then(|path| path.to_str()),
            backend: request.backend.as_deref(),
            skip_dependencies: request.skip_dependencies,
            continue_on_error: request.continue_on_error,
            dry_run: request.dry_run,
            executor: request.executor,
            task_name,
            labels,
            task_args,
        }
    }

    fn should_show_general_help(&self) -> bool {
        self.task_name.is_none() && self.output.help
    }

    fn uses_interactive_picker(&self) -> bool {
        matches!(self.selection, TaskSelection::Interactive)
            && self.task_name.is_none()
            && self.labels.is_empty()
    }

    fn lists_tasks(&self) -> bool {
        self.task_name.is_none() && self.labels.is_empty()
    }

    fn shows_cache_path(&self) -> bool {
        self.output.show_cache_path
    }

    fn shows_help(&self) -> bool {
        self.output.help
    }

    fn uses_tui(&self) -> bool {
        self.execution_mode == &ExecutionMode::Tui
    }
}

struct TaskExecutionContext {
    manifest: Project,
    project_root: PathBuf,
    cue_module_root: Option<PathBuf>,
    task_index: TaskIndex,
    local_tasks: Tasks,
}

struct PreparedTaskRuntime {
    env: Environment,
    cache: Option<TaskCacheConfig>,
}

fn load_task_execution_context(input: &TaskExecutionInput<'_>) -> Result<TaskExecutionContext> {
    let mut manifest: Project =
        evaluate_manifest(Path::new(input.path), input.package, input.executor)?;
    tracing::debug!("CUE evaluation successful");
    tracing::debug!(
        "Successfully parsed CUE evaluation, found {} tasks",
        manifest.tasks.len()
    );

    let project_root =
        std::fs::canonicalize(input.path).unwrap_or_else(|_| Path::new(input.path).to_path_buf());
    let cue_module_root = find_cue_module_root(&project_root);
    let task_index = prepare_task_index(&mut manifest, &project_root)?;
    let local_tasks = task_index.to_tasks();

    Ok(TaskExecutionContext {
        manifest,
        project_root,
        cue_module_root,
        task_index,
        local_tasks,
    })
}

/// Internal implementation of task execution.
pub(super) async fn execute_task_impl(request: &TaskExecutionRequest<'_>) -> Result<String> {
    let input = TaskExecutionInput::from_request(request);

    // Handle CLI help immediately if no task specified
    if input.should_show_general_help() {
        return Ok(get_task_cli_help());
    }

    tracing::info!(
        "Executing task from path: {}, package: {}, task: {:?}",
        input.path,
        input.package,
        input.task_name
    );

    let context = load_task_execution_context(&input)?;

    if let Some(output) = maybe_run_interactive_picker(&input, &context).await? {
        return Ok(output);
    }

    if let Some(output) = maybe_render_task_list(&input, &context)? {
        return Ok(output);
    }

    let normalized_labels = validate_task_selection(&input)?;
    if let Some(output) = maybe_render_task_help(&input, &context)? {
        return Ok(output);
    }
    let resolution = resolve_task_selection(&input, &context, &normalized_labels)?;

    let task_graph = build_execution_graph(&input, &resolution)?;

    // Handle dry-run mode: export DAG as JSON without executing
    if input.dry_run.is_dry_run() {
        return export_task_graph(&task_graph);
    }

    let runtime = prepare_task_runtime(&input, &context, &resolution).await?;
    execute_resolved_task(TaskRunRequest {
        input: &input,
        context: &context,
        resolution: &resolution,
        task_graph: &task_graph,
        runtime,
    })
    .await
}

fn maybe_render_task_help(
    input: &TaskExecutionInput<'_>,
    context: &TaskExecutionContext,
) -> Result<Option<String>> {
    if !input.shows_help() {
        return Ok(None);
    }

    let requested_task = input.task_name.ok_or_else(|| {
        cuenv_core::Error::configuration("task name required when no labels provided")
    })?;
    let prefix = format!("{requested_task}.");
    let subtasks: Vec<&cuenv_core::tasks::IndexedTask> = context
        .task_index
        .list()
        .iter()
        .filter(|task| task.name == requested_task || task.name.starts_with(&prefix))
        .copied()
        .collect();

    if subtasks.is_empty() {
        return Err(cuenv_core::Error::configuration(format!(
            "Task '{requested_task}' not found",
        )));
    }

    if subtasks.len() == 1 && subtasks[0].name == requested_task {
        return Ok(Some(format_task_detail(subtasks[0])));
    }

    Ok(Some(render_task_tree(subtasks, None)))
}

fn build_execution_graph(
    input: &TaskExecutionInput<'_>,
    resolution: &TaskResolution,
) -> Result<TaskGraph> {
    tracing::debug!(
        "Building task graph for task: {}",
        resolution.graph_root_name
    );
    let mut task_graph = TaskGraph::new();

    if input.skip_dependencies {
        tracing::debug!("Skipping dependencies - adding only the target task");
        if let Some(TaskNode::Task(task)) = resolution.tasks.get(&resolution.graph_root_name) {
            task_graph.add_task(&resolution.graph_root_name, (**task).clone())?;
        }
    } else {
        task_graph
            .build_for_task(&resolution.graph_root_name, &resolution.tasks)
            .map_err(|e| {
                tracing::error!("Failed to build task graph: {}", e);
                e
            })?;
    }

    if !resolution.output_ref_deps.is_empty() {
        task_graph.add_output_ref_deps(&resolution.output_ref_deps, &resolution.tasks)?;
    }

    tracing::debug!(
        "Successfully built task graph with {} tasks",
        task_graph.task_count()
    );

    Ok(task_graph)
}

fn export_task_graph(task_graph: &TaskGraph) -> Result<String> {
    let dag_export = dag_export::DagExport::from_task_graph(task_graph)?;
    serde_json::to_string_pretty(&dag_export)
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to serialize DAG: {e}")))
}

async fn prepare_task_runtime(
    input: &TaskExecutionInput<'_>,
    context: &TaskExecutionContext,
    resolution: &TaskResolution,
) -> Result<PreparedTaskRuntime> {
    let directory = context.project_root.clone();
    let base_env_vars = get_environment_with_hooks(
        HookEnvironmentRequest::new(&directory, &context.manifest, input.package)
            .with_executor(input.executor),
    )
    .await?;

    let mut runtime_env = base_runtime_environment(context).await?;
    apply_task_environment(TaskEnvironmentApplication {
        input,
        context,
        resolution,
        runtime_env: &mut runtime_env,
        base_env_vars,
    })
    .await?;
    activate_lockfile_tools(&context.manifest, &context.project_root, &mut runtime_env).await?;

    Ok(PreparedTaskRuntime {
        env: runtime_env,
        cache: task_cache_for_context(context),
    })
}

async fn base_runtime_environment(context: &TaskExecutionContext) -> Result<Environment> {
    let mut runtime_env = Environment::new();
    let runtime_env_vars =
        resolve_runtime_environment(&context.project_root, context.manifest.runtime.as_ref())
            .await?;
    for (key, value) in runtime_env_vars {
        runtime_env.set(key, value);
    }
    Ok(runtime_env)
}

struct TaskEnvironmentApplication<'a, 'input> {
    input: &'a TaskExecutionInput<'input>,
    context: &'a TaskExecutionContext,
    resolution: &'a TaskResolution,
    runtime_env: &'a mut Environment,
    base_env_vars: HashMap<String, String>,
}

async fn apply_task_environment(application: TaskEnvironmentApplication<'_, '_>) -> Result<()> {
    if let Some(env) = &application.context.manifest.env {
        for (key, value) in &application.base_env_vars {
            application.runtime_env.set(key.clone(), value.clone());
        }

        let env_vars = if let Some(env_name) = application.input.environment {
            env.for_environment(env_name)
        } else {
            env.base.clone()
        };

        let (task_env_vars, secrets) =
            cuenv_core::environment::Environment::resolve_for_task_with_secrets(
                application.resolution.display_name.as_str(),
                &env_vars,
            )
            .await?;

        cuenv_events::register_secrets(secrets);

        for (key, value) in task_env_vars {
            application.runtime_env.set(key, value);
        }
    } else {
        for (key, value) in application.base_env_vars {
            application.runtime_env.set(key, value);
        }
    }

    Ok(())
}

async fn activate_lockfile_tools(
    manifest: &Project,
    project_root: &Path,
    runtime_env: &mut Environment,
) -> Result<()> {
    if should_activate_lockfile_tools(manifest) {
        ensure_tools_downloaded(Some(project_root))
            .await
            .map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to download tools: {e}"))
            })?;
        if let Some(activation_steps) =
            resolve_tool_activation_steps(Some(project_root)).map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to resolve tools activation: {e}"))
            })?
        {
            tracing::debug!(
                steps = activation_steps.len(),
                "Applying configured tool activation operations for task execution"
            );

            for step in activation_steps {
                let current = runtime_env.get(&step.var);
                if let Some(new_value) = apply_resolved_tool_activation(current, &step) {
                    runtime_env.set(step.var.clone(), new_value);
                }
            }
        }
    }

    Ok(())
}

fn should_activate_lockfile_tools(project: &Project) -> bool {
    matches!(project.runtime, Some(Runtime::Tools(_)))
}

fn task_cache_for_context(context: &TaskExecutionContext) -> Option<TaskCacheConfig> {
    let module_root = context
        .cue_module_root
        .as_deref()
        .unwrap_or(context.project_root.as_path());
    let runtime_identity = resolve_runtime_cache_identity(
        module_root,
        context.project_root.as_path(),
        context.manifest.runtime.as_ref(),
    );
    if let Some(reason) = &runtime_identity.cache_disabled_reason {
        tracing::warn!(reason, "task cache disabled for this invocation");
    }
    build_task_cache(&context.project_root, runtime_identity)
}

struct TaskRunRequest<'a, 'input> {
    input: &'a TaskExecutionInput<'input>,
    context: &'a TaskExecutionContext,
    resolution: &'a TaskResolution,
    task_graph: &'a TaskGraph,
    runtime: PreparedTaskRuntime,
}

async fn execute_resolved_task(request: TaskRunRequest<'_, '_>) -> Result<String> {
    let executor_config = task_executor_config(&TaskExecutorConfigSpec {
        input: request.input,
        context: request.context,
        runtime: &request.runtime,
        capture_output: request.input.capture_output,
    });
    let executor = TaskExecutor::with_dagger_factory(executor_config, get_dagger_factory());

    if request.input.uses_tui() && request.task_graph.task_count() > 0 {
        let tui_config = task_executor_config(&TaskExecutorConfigSpec {
            input: request.input,
            context: request.context,
            runtime: &request.runtime,
            capture_output: OutputCapture::Capture,
        });
        let tui_executor = TaskExecutor::with_dagger_factory(tui_config, get_dagger_factory());

        return execute_with_rich_tui(
            &tui_executor,
            request.resolution.display_name.as_str(),
            request.task_graph,
        )
        .await;
    }

    let results = execute_task_with_strategy(
        &executor,
        request.resolution.display_name.as_str(),
        &request.resolution.node,
        request.task_graph,
        &request.resolution.tasks,
    )
    .await?;

    if let Some(failed) = results.iter().find(|result| !result.success) {
        return Err(cuenv_core::Error::configuration(summarize_task_failure(
            failed,
            TASK_FAILURE_SNIPPET_LINES,
        )));
    }

    Ok(format_task_results(
        results,
        request.input.capture_output,
        request.resolution.display_name.as_str(),
    ))
}

struct TaskExecutorConfigSpec<'a, 'input> {
    input: &'a TaskExecutionInput<'input>,
    context: &'a TaskExecutionContext,
    runtime: &'a PreparedTaskRuntime,
    capture_output: OutputCapture,
}

fn task_executor_config(spec: &TaskExecutorConfigSpec<'_, '_>) -> ExecutorConfig {
    ExecutorConfig {
        capture_output: spec.capture_output,
        max_parallel: 0,
        continue_on_error: spec.input.continue_on_error,
        environment: spec.runtime.env.clone(),
        working_dir: None,
        cue_module_root: spec.context.cue_module_root.clone(),
        project_root: spec.context.project_root.clone(),
        materialize_outputs: spec
            .input
            .materialize_outputs
            .map(|path| Path::new(path).to_path_buf()),
        cache_dir: None,
        show_cache_path: spec.input.shows_cache_path(),
        backend_config: spec
            .context
            .manifest
            .config
            .as_ref()
            .and_then(|config| config.backend.clone()),
        cli_backend: spec.input.backend.map(ToString::to_string),
        cache: spec.runtime.cache.clone(),
    }
}
