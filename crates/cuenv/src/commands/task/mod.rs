//! Task execution command implementation

mod arguments;
mod dag_export;
mod discovery;
pub mod list_builder;
mod rendering;
mod types;

// Re-export types for the public API. Some types may not be used externally yet.
#[allow(unused_imports)]
pub use types::{ExecutionMode, OutputConfig, TaskExecutionRequest, TaskSelection};

use arguments::{apply_args_to_task, resolve_task_args};
use discovery::{evaluate_manifest, find_tasks_with_labels, format_label_root, normalize_labels};
use list_builder::prepare_task_index;
use rendering::{format_task_detail, get_task_cli_help, render_task_tree};

use cuenv_core::environment::Environment;
use cuenv_core::lockfile::{LOCKFILE_NAME, LockedRuntime, Lockfile};
use cuenv_core::manifest::{Project, Runtime};
use cuenv_core::tasks::cache::TaskCacheConfig;
use cuenv_core::tasks::executor::{TASK_FAILURE_SNIPPET_LINES, summarize_task_failure};
use cuenv_core::tasks::{
    BackendFactory, ExecutorConfig, Task, TaskExecutor, TaskGraph, TaskIndex, TaskNode, Tasks,
};
use cuenv_core::tools::apply_resolved_tool_activation;
use cuenv_core::{DryRun, OutputCapture, Result};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use super::env_file::find_cue_module_root;
use super::relative_path_from_root;
use super::task_list::{
    DashboardFormatter, EmojiFormatter, RichFormatter, TablesFormatter, TaskListFormatter,
    TextFormatter, build_task_list,
};
use super::task_picker::{PickerResult, SelectableTask, run_picker};
use super::tools::{ensure_tools_downloaded, resolve_tool_activation_steps};
use crate::tui::rich::RichTui;
use crate::tui::state::TaskInfo;
use cuenv_core::runtime::resolve_runtime_environment;
use std::io::IsTerminal;

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
use std::path::{Path, PathBuf};

use super::export::{HookEnvironmentRequest, get_environment_with_hooks};
use tracing::instrument;

/// Resolve the on-disk root for the local CAS + action cache.
///
/// Resolution order:
/// 1. `$CUENV_CACHE_DIR` (explicit override)
/// 2. `$XDG_CACHE_HOME/cuenv` or the platform default
/// 3. `<project>/.cuenv-cache`
fn resolve_cache_root(project_root: &Path) -> PathBuf {
    if let Some(env) = std::env::var_os("CUENV_CACHE_DIR")
        && !env.is_empty()
    {
        return PathBuf::from(env);
    }
    if let Some(d) = dirs::cache_dir() {
        return d.join("cuenv");
    }
    project_root.join(".cuenv-cache")
}

/// Construct the [`TaskCacheConfig`] used by the executor.
///
/// Returns `None` if the local CAS or action cache cannot be opened (e.g.
/// permissions). In that case the executor falls back to the no-cache code
/// path so the user's command still works — degraded, not broken.
fn build_task_cache(
    project_root: &Path,
    runtime_identity: RuntimeCacheIdentity,
) -> Option<TaskCacheConfig> {
    let root = resolve_cache_root(project_root);
    let cas = match cuenv_cas::LocalCas::open(&root) {
        Ok(c) => Arc::new(c) as Arc<dyn cuenv_cas::Cas>,
        Err(e) => {
            tracing::warn!(error = %e, root = %root.display(), "task cache disabled: cannot open CAS");
            return None;
        }
    };
    let action_cache = match cuenv_cas::LocalActionCache::open(&root) {
        Ok(ac) => Arc::new(ac) as Arc<dyn cuenv_cas::ActionCache>,
        Err(e) => {
            tracing::warn!(error = %e, root = %root.display(), "task cache disabled: cannot open action cache");
            return None;
        }
    };
    let vcs_hasher =
        Arc::new(cuenv_vcs::WalkHasher::new(project_root)) as Arc<dyn cuenv_vcs::VcsHasher>;
    Some(TaskCacheConfig {
        cas,
        action_cache,
        vcs_hasher,
        vcs_hasher_root: project_root.to_path_buf(),
        cuenv_version: env!("CARGO_PKG_VERSION").to_string(),
        runtime_identity_properties: runtime_identity.properties,
        cache_disabled_reason: runtime_identity.cache_disabled_reason,
    })
}

#[derive(Debug, Clone, Default)]
struct RuntimeCacheIdentity {
    properties: BTreeMap<String, String>,
    cache_disabled_reason: Option<String>,
}

fn resolve_runtime_cache_identity(
    module_root: &Path,
    project_root: &Path,
    runtime: Option<&Runtime>,
) -> RuntimeCacheIdentity {
    let mut identity = RuntimeCacheIdentity::default();
    let Some(runtime) = runtime else {
        return identity;
    };

    match runtime {
        Runtime::Nix(nix_runtime) => {
            identity
                .properties
                .insert("runtime.kind".to_string(), "nix".to_string());

            let lockfile_path = module_root.join(LOCKFILE_NAME);
            let lockfile = match Lockfile::load(&lockfile_path) {
                Ok(Some(lockfile)) => lockfile,
                Ok(None) => {
                    identity.cache_disabled_reason = Some(format!(
                        "runtime is nix but {} is missing",
                        lockfile_path.display()
                    ));
                    return identity;
                }
                Err(e) => {
                    identity.cache_disabled_reason = Some(format!(
                        "runtime is nix but {} could not be read: {}",
                        lockfile_path.display(),
                        e
                    ));
                    return identity;
                }
            };

            let project_path = relative_path_from_root(module_root, project_root);
            let project_key = project_path.to_string_lossy().into_owned();
            let Some(locked_runtime) = lockfile.find_runtime(&project_key) else {
                identity.cache_disabled_reason = Some(format!(
                    "runtime is nix but lockfile has no runtime entry for project '{}'",
                    project_key
                ));
                return identity;
            };

            let LockedRuntime::Nix(locked_nix) = locked_runtime;

            if locked_nix.flake != nix_runtime.flake || locked_nix.output != nix_runtime.output {
                identity.cache_disabled_reason = Some(format!(
                    "runtime lock mismatch for project '{}': expected flake='{}' output='{}', got flake='{}' output='{}'",
                    project_key,
                    nix_runtime.flake,
                    nix_runtime.output.as_deref().unwrap_or(""),
                    locked_nix.flake,
                    locked_nix.output.as_deref().unwrap_or("")
                ));
                return identity;
            }

            identity
                .properties
                .insert("runtime.nix.digest".to_string(), locked_nix.digest.clone());
            identity
                .properties
                .insert("runtime.nix.flake".to_string(), locked_nix.flake.clone());
            if let Some(output) = &locked_nix.output {
                identity
                    .properties
                    .insert("runtime.nix.output".to_string(), output.clone());
            }
            identity.properties.insert(
                "runtime.nix.lockfile".to_string(),
                locked_nix.lockfile.clone(),
            );
            identity
        }
        Runtime::Devenv(_) => {
            identity
                .properties
                .insert("runtime.kind".to_string(), "devenv".to_string());
            identity
        }
        Runtime::Container(_) => {
            identity
                .properties
                .insert("runtime.kind".to_string(), "container".to_string());
            identity
        }
        Runtime::Dagger(_) => {
            identity
                .properties
                .insert("runtime.kind".to_string(), "dagger".to_string());
            identity
        }
        Runtime::Oci(_) => {
            identity
                .properties
                .insert("runtime.kind".to_string(), "oci".to_string());
            identity
        }
        Runtime::Tools(_) => {
            identity
                .properties
                .insert("runtime.kind".to_string(), "tools".to_string());
            identity
        }
    }
}

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
#[instrument(name = "task_execute", skip(request), fields(path = %request.path, package = %request.package))]
pub async fn execute(request: TaskExecutionRequest<'_>) -> Result<String> {
    execute_task_impl(&request).await
}

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
async fn execute_task_impl(request: &TaskExecutionRequest<'_>) -> Result<String> {
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

fn should_activate_lockfile_tools(project: &Project) -> bool {
    matches!(project.runtime, Some(Runtime::Tools(_)))
}

/// Execute task with rich TUI interface
///
/// Note: The executor MUST have `capture_output: true` to ensure task output
/// goes through the event system rather than directly to stdout/stderr.
async fn execute_with_rich_tui(
    executor: &TaskExecutor,
    task_name: &str,
    task_graph: &TaskGraph,
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
            .filter_map(|dep| levels.get(dep.task_name()).copied())
            .max()
            .unwrap_or(0);
        let increment = usize::from(!node.task.depends_on.is_empty());
        levels.insert(node.name.clone(), max_dep_level.saturating_add(increment));
    }

    for node in sorted_tasks {
        let task_name = node.name.clone();
        let dependencies: Vec<String> = node
            .task
            .depends_on
            .iter()
            .map(|d| d.task_name().to_string())
            .collect();
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
    task_node: &TaskNode,
    task_graph: &TaskGraph,
    all_tasks: &Tasks,
) -> Result<Vec<cuenv_core::tasks::TaskResult>> {
    match task_node {
        TaskNode::Group(_) | TaskNode::Sequence(_) => {
            // For groups (parallel) and lists (sequential), use the original execution
            executor.execute_node(task_name, task_node, all_tasks).await
        }
        TaskNode::Task(_) => {
            // The task graph is built from `all_tasks` and is the authoritative
            // dependency view for execution.
            if task_graph.task_count() <= 1 {
                executor.execute_node(task_name, task_node, all_tasks).await
            } else {
                executor.execute_graph(task_graph).await
            }
        }
    }
}

fn format_task_results(
    results: Vec<cuenv_core::tasks::TaskResult>,
    capture_output: cuenv_core::OutputCapture,
    task_name: &str,
) -> String {
    let mut output = String::new();
    for result in results {
        if capture_output.should_capture() {
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

    if capture_output.should_capture() && output.is_empty() {
        output = format!("Task '{task_name}' completed");
    } else if !capture_output.should_capture() {
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
#[path = "mod_tests.rs"]
mod tests;
