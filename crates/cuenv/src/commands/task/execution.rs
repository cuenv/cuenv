use super::arguments::{apply_args_to_task, resolve_task_args};
use super::dag_export;
use super::discovery::{
    evaluate_manifest, find_tasks_with_labels, format_label_root, normalize_labels,
};
use super::list_builder::prepare_task_index;
use super::rendering::{format_task_detail, get_task_cli_help, render_task_tree};
use super::types::{ExecutionMode, OutputConfig, TaskExecutionRequest, TaskSelection};
use super::{
    build_task_cache, execute, execute_task_with_strategy, execute_with_rich_tui,
    format_task_results, get_dagger_factory, resolve_runtime_cache_identity,
};
use crate::commands::env_file::find_cue_module_root;
use crate::commands::export::{HookEnvironmentRequest, get_environment_with_hooks};
use crate::commands::relative_path_from_root;
use crate::commands::task_list::{
    DashboardFormatter, EmojiFormatter, RichFormatter, TablesFormatter, TaskListFormatter,
    TextFormatter, build_task_list,
};
use crate::commands::task_picker::{PickerResult, SelectableTask, run_picker};
use crate::commands::tools::{ensure_tools_downloaded, resolve_tool_activation_steps};
use cuenv_core::environment::Environment;
use cuenv_core::manifest::{Project, Runtime};
use cuenv_core::runtime::resolve_runtime_environment;
use cuenv_core::tasks::cache::TaskCacheConfig;
use cuenv_core::tasks::executor::{TASK_FAILURE_SNIPPET_LINES, summarize_task_failure};
use cuenv_core::tasks::{
    ExecutorConfig, Task, TaskExecutor, TaskGraph, TaskIndex, TaskNode, Tasks,
};
use cuenv_core::tools::apply_resolved_tool_activation;
use cuenv_core::{DryRun, OutputCapture, Result};
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

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

/// Resolved task context from either named-task or label-based resolution.
struct TaskResolution {
    display_name: String,
    node: TaskNode,
    tasks: Tasks,
    graph_root_name: String,
    output_ref_deps: Vec<(String, String)>,
}

struct PreparedTaskRuntime {
    env: Environment,
    cache: Option<TaskCacheConfig>,
}

fn current_instance_output_ref_deps(
    executor: &crate::commands::CommandExecutor,
    project_root: &Path,
) -> Result<Vec<(String, String)>> {
    let module = executor.get_module(project_root)?;
    let rel_path = relative_path_from_root(&module.root, project_root);

    Ok(module
        .get(&rel_path)
        .map_or_else(Vec::new, |instance| instance.output_ref_deps.clone()))
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

async fn maybe_run_interactive_picker(
    input: &TaskExecutionInput<'_>,
    context: &TaskExecutionContext,
) -> Result<Option<String>> {
    if !input.uses_interactive_picker() {
        return Ok(None);
    }

    let selectable = context
        .task_index
        .list()
        .iter()
        .map(|task| SelectableTask {
            name: task.name.clone(),
            description: task_description(&task.node),
        })
        .collect();

    match run_picker(selectable) {
        Ok(PickerResult::Selected(selected_task)) => {
            let request = selected_task_request(input, &selected_task);
            Box::pin(execute(request)).await.map(Some)
        }
        Ok(PickerResult::Cancelled) => Ok(Some(String::new())),
        Err(e) => Err(cuenv_core::Error::configuration(format!(
            "Interactive picker failed: {e}"
        ))),
    }
}

fn task_description(node: &TaskNode) -> Option<String> {
    match node {
        TaskNode::Task(task) => task.description.clone(),
        TaskNode::Group(group) => group.description.clone(),
        TaskNode::Sequence(_) => None,
    }
}

fn selected_task_request<'a>(
    input: &TaskExecutionInput<'a>,
    selected_task: &str,
) -> TaskExecutionRequest<'a> {
    let mut request =
        TaskExecutionRequest::named(input.path, input.package, selected_task, input.executor)
            .with_format(input.format);

    if let Some(env) = input.environment {
        request = request.with_environment(env);
    }
    if input.capture_output.should_capture() {
        request = request.with_capture();
    }
    if let Some(path) = input.materialize_outputs {
        request = request.with_materialize_outputs(path);
    }
    if input.shows_cache_path() {
        request = request.with_show_cache_path();
    }
    if let Some(backend) = input.backend {
        request = request.with_backend(backend);
    }
    if input.uses_tui() {
        request = request.with_tui();
    }
    if input.shows_help() {
        request = request.with_help();
    }
    if input.skip_dependencies {
        request = request.with_skip_dependencies();
    }

    request
}

fn maybe_render_task_list(
    input: &TaskExecutionInput<'_>,
    context: &TaskExecutionContext,
) -> Result<Option<String>> {
    if !input.lists_tasks() {
        return Ok(None);
    }

    tracing::debug!("Listing available tasks");
    let tasks = context.task_index.list();
    tracing::debug!("Found {} tasks to list", tasks.len());

    if input.format == "json" {
        return serde_json::to_string(&tasks).map(Some).map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to serialize tasks: {e}"))
        });
    }

    if tasks.is_empty() {
        return Ok(Some("No tasks defined in the configuration".to_string()));
    }

    let cwd_relative = context.cue_module_root.as_ref().and_then(|root| {
        context
            .project_root
            .strip_prefix(root)
            .ok()
            .map(|path| path.to_string_lossy().to_string())
    });
    let task_data = build_task_list(&tasks, cwd_relative.as_deref(), &context.project_root);
    let effective_format = if input.format.is_empty() {
        context
            .manifest
            .config
            .as_ref()
            .and_then(|config| config.task_list_format())
            .map(|format| format.as_str())
    } else {
        Some(input.format)
    };

    let output = match effective_format {
        Some("rich") => RichFormatter::new().format(&task_data),
        Some("text") => TextFormatter.format(&task_data),
        Some("tables") => TablesFormatter::new().format(&task_data),
        Some("dashboard") => DashboardFormatter::new().format(&task_data),
        Some("emoji") => EmojiFormatter.format(&task_data),
        _ if std::io::stdout().is_terminal() => RichFormatter::new().format(&task_data),
        _ => TextFormatter.format(&task_data),
    };

    Ok(Some(output))
}

fn validate_task_selection(input: &TaskExecutionInput<'_>) -> Result<Vec<String>> {
    if !input.labels.is_empty() && input.task_name.is_some() {
        return Err(cuenv_core::Error::configuration(
            "Cannot specify both a task name and --label",
        ));
    }
    if !input.labels.is_empty() && !input.task_args.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "Task arguments are not supported when selecting tasks by label",
        ));
    }

    let normalized_labels = normalize_labels(input.labels);
    if !input.labels.is_empty() && normalized_labels.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "Labels cannot be empty or whitespace-only",
        ));
    }

    Ok(normalized_labels)
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

fn resolve_task_selection(
    input: &TaskExecutionInput<'_>,
    context: &TaskExecutionContext,
    normalized_labels: &[String],
) -> Result<TaskResolution> {
    if normalized_labels.is_empty() {
        resolve_named_task(input, context)
    } else {
        resolve_label_tasks(input, context, normalized_labels)
    }
}

fn resolve_named_task(
    input: &TaskExecutionInput<'_>,
    context: &TaskExecutionContext,
) -> Result<TaskResolution> {
    let requested_task = input.task_name.ok_or_else(|| {
        cuenv_core::Error::configuration("task name required when no labels provided")
    })?;
    tracing::debug!("Looking for specific task: {}", requested_task);

    let task_entry = context.task_index.resolve(requested_task)?;
    let display_task_name = task_entry.name.clone();
    log_task_resolution_context(context, requested_task);
    let original_task_node = context.local_tasks.get(&display_task_name).ok_or_else(|| {
        cuenv_core::Error::configuration(format!("Task '{display_task_name}' not found"))
    })?;
    tracing::debug!("Found task node: {:?}", original_task_node);

    let mut tasks_in_scope = context.local_tasks.clone();
    let selected_task_node = selected_task_node(input, &mut tasks_in_scope, &display_task_name)?;

    Ok(TaskResolution {
        display_name: display_task_name.clone(),
        node: selected_task_node,
        tasks: tasks_in_scope,
        graph_root_name: display_task_name,
        output_ref_deps: current_instance_output_ref_deps(input.executor, &context.project_root)?,
    })
}

fn log_task_resolution_context(context: &TaskExecutionContext, requested_task: &str) {
    tracing::debug!(
        "Task index entries: {:?}",
        context
            .task_index
            .list()
            .iter()
            .map(|task| task.name.as_str())
            .collect::<Vec<_>>()
    );
    tracing::debug!(
        "Indexed tasks for execution: {:?}",
        context.local_tasks.list_tasks()
    );
    tracing::debug!(
        "Requested task '{}' present: {}",
        requested_task,
        context.local_tasks.get(requested_task).is_some()
    );
}

fn selected_task_node(
    input: &TaskExecutionInput<'_>,
    tasks_in_scope: &mut Tasks,
    display_task_name: &str,
) -> Result<TaskNode> {
    let node = tasks_in_scope
        .get(display_task_name)
        .cloned()
        .ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "Task '{display_task_name}' not found in local tasks"
            ))
        })?;

    if input.task_args.is_empty() {
        return Ok(node);
    }

    let TaskNode::Task(task) = node else {
        return Err(cuenv_core::Error::configuration(
            "Task arguments are not supported for task groups or lists".to_string(),
        ));
    };

    let resolved_args = resolve_task_args(task.params.as_ref(), input.task_args)?;
    tracing::debug!("Resolved task args: {:?}", resolved_args);

    let modified_task = apply_args_to_task(&task, &resolved_args);
    let modified_node = TaskNode::Task(Box::new(modified_task.clone()));
    tasks_in_scope
        .tasks
        .insert(display_task_name.to_string(), modified_node.clone());
    Ok(modified_node)
}

fn resolve_label_tasks(
    input: &TaskExecutionInput<'_>,
    context: &TaskExecutionContext,
    normalized_labels: &[String],
) -> Result<TaskResolution> {
    let mut tasks_in_scope = context.local_tasks.clone();
    let matching_tasks = find_tasks_with_labels(&context.local_tasks, normalized_labels);

    if matching_tasks.is_empty() {
        return Err(cuenv_core::Error::configuration(format!(
            "No tasks with labels {normalized_labels:?} were found in this scope"
        )));
    }

    let display_task_name = format_label_root(normalized_labels);
    let synthetic = Task {
        script: Some("true".to_string()),
        hermetic: false,
        depends_on: matching_tasks
            .into_iter()
            .map(cuenv_core::tasks::TaskDependency::from_name)
            .collect(),
        project_root: Some(context.project_root.clone()),
        description: Some(format!(
            "Run all tasks matching labels: {}",
            normalized_labels.join(", ")
        )),
        ..Default::default()
    };

    tasks_in_scope.tasks.insert(
        display_task_name.clone(),
        TaskNode::Task(Box::new(synthetic)),
    );

    let resolved_node = tasks_in_scope
        .get(&display_task_name)
        .cloned()
        .ok_or_else(|| cuenv_core::Error::execution("synthetic task missing after insertion"))?;

    Ok(TaskResolution {
        display_name: display_task_name.clone(),
        node: resolved_node,
        tasks: tasks_in_scope,
        graph_root_name: display_task_name,
        output_ref_deps: current_instance_output_ref_deps(input.executor, &context.project_root)?,
    })
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
