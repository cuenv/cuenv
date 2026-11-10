//! Task execution orchestration.

use super::super::cache::TaskCacheContext;
use super::super::dag_export;

use super::super::runner::StrategyInput;
use super::super::types::TaskExecutionRequest;
use super::super::{list_builder, rendering, runner};
use crate::commands::CommandExecutor;

use crate::commands::git_hooks;

use cuenv_core::context::{Consumer, ContextDefinition, Facts, TaskSubject};
use cuenv_core::environment::{Env, EnvSelection, EnvValue, Environment};
use cuenv_core::manifest::{Project, Runtime};

use cuenv_core::tasks::{Task, TaskNode, Tasks};
use cuenv_core::{OutputCapture, Result};
use cuenv_task_exec::cache::TaskCacheConfig;
use cuenv_task_exec::executor::TASK_FAILURE_SNIPPET_LINES;
use cuenv_task_exec::{
    ExecutorConfig, TaskEnvironmentFuture, TaskEnvironments, TaskExecutor, TaskGraph,
};
use cuenv_tool_runtime::ResolvedToolActivationStep;

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, OnceCell};

use super::context::{TaskExecutionContext, TaskExecutionInput};
use super::selection::TaskResolution;
use super::{help, listing, picker, selection};

struct PreparedTaskRuntime {
    env: Environment,
    /// Environments for the tasks a context selects other variables for.
    task_environments: Option<Arc<dyn TaskEnvironments>>,
    cache: Option<TaskCacheConfig>,
}

fn load_task_execution_context(input: &TaskExecutionInput<'_>) -> Result<TaskExecutionContext> {
    let mut manifest: Project = super::super::discovery::evaluate_manifest(
        Path::new(input.path),
        input.package,
        input.executor,
    )?;
    tracing::debug!("CUE evaluation successful");
    tracing::debug!(
        "Successfully parsed CUE evaluation, found {} tasks",
        manifest.tasks.len()
    );

    let project_root =
        std::fs::canonicalize(input.path).unwrap_or_else(|_| Path::new(input.path).to_path_buf());
    let cue_module_root = crate::commands::env_file::find_cue_module_root(&project_root);
    let task_index = list_builder::prepare_task_index(&mut manifest, &project_root)?;
    let mut local_tasks = task_index.to_tasks();
    let project_roots =
        extend_task_scope_with_cross_project(input.executor, &project_root, &mut local_tasks);

    Ok(TaskExecutionContext {
        manifest,
        project_root,
        cue_module_root,
        project_roots,
        task_index,
        local_tasks,
    })
}

/// Map every project in the CUE module to its absolute root.
///
/// A `#ProjectReference` may spell its target as the project's `name` or as
/// its path relative to the module root, so both spellings are indexed rather
/// than guessing which one a user meant. A module that cannot be evaluated
/// yields an empty map: cross-project inputs then cannot be hashed, and the
/// tasks that declare them are reported as uncacheable instead of being
/// keyed on a fraction of their inputs.
fn discover_project_roots(
    executor: &CommandExecutor,
    project_root: &Path,
) -> BTreeMap<String, PathBuf> {
    match executor.module(project_root) {
        Ok(module) => project_roots_from_module(&module),
        Err(error) => {
            tracing::debug!(
                error = %error,
                "cross-project inputs unavailable: module evaluation failed"
            );
            BTreeMap::new()
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct ExternalProjectRequest {
    declared_project: String,
    origin_root: PathBuf,
}

/// Load every project the local tasks reference, and add its tasks to scope.
///
/// This runs before any task is selected, so a reference that cannot be
/// loaded is a warning here, not an error: it would otherwise stop every
/// `cuenv task` in the project, including ones that never touch it. The task
/// that declares it still fails where it is used, because the dependency it
/// implies (`#project:task`) is then missing from the graph.
fn extend_task_scope_with_cross_project(
    executor: &CommandExecutor,
    project_root: &Path,
    tasks: &mut Tasks,
) -> BTreeMap<String, PathBuf> {
    let mut pending = VecDeque::from(collect_project_requests(tasks, project_root));
    if pending.is_empty() {
        // Nothing references another project, so there is nothing to map
        // and no reason to evaluate the module for it.
        return BTreeMap::new();
    }

    let mut project_roots = discover_project_roots(executor, project_root);
    match executor.discover_all_modules(project_root) {
        Ok(workspace) => project_roots.extend(project_roots_from_module(&workspace)),
        Err(error) => tracing::warn!(
            %error,
            "workspace discovery failed; cross-project references resolve by path only"
        ),
    }

    let mut loaded_aliases = BTreeMap::<String, PathBuf>::new();
    while let Some(request) = pending.pop_front() {
        let loaded = load_external_project(ExternalProjectLoad {
            request: &request,
            project_roots: &mut project_roots,
            loaded_aliases: &loaded_aliases,
        });
        match loaded {
            Ok(ExternalProjectOutcome::AlreadyLoaded) => {}
            Ok(ExternalProjectOutcome::Loaded {
                root,
                tasks: external_tasks,
            }) => {
                pending.extend(collect_project_requests(&external_tasks, &root));
                for (task_name, node) in external_tasks.tasks {
                    let qualified_name = format!("#{}:{task_name}", request.declared_project);
                    let qualified_node =
                        qualify_external_task_node(node, &request.declared_project, &root);
                    tasks.tasks.insert(qualified_name, qualified_node);
                }
                loaded_aliases.insert(request.declared_project, root);
            }
            Err(error) => {
                tracing::warn!(
                    project = request.declared_project,
                    %error,
                    "cross-project reference could not be loaded; tasks that use it will fail"
                );
            }
        }
    }

    project_roots
}

struct ExternalProjectLoad<'external_project_load> {
    request: &'external_project_load ExternalProjectRequest,
    project_roots: &'external_project_load mut BTreeMap<String, PathBuf>,
    loaded_aliases: &'external_project_load BTreeMap<String, PathBuf>,
}

enum ExternalProjectOutcome {
    /// The alias already names this project; its tasks are in scope.
    AlreadyLoaded,
    /// A newly evaluated project and its tasks, not yet qualified.
    Loaded { root: PathBuf, tasks: Tasks },
}

fn load_external_project(load: ExternalProjectLoad<'_>) -> Result<ExternalProjectOutcome> {
    let ExternalProjectLoad {
        request,
        project_roots,
        loaded_aliases,
    } = load;

    let external_root = resolve_external_project_root(project_roots, request)?;
    if let Some(previous_root) = loaded_aliases.get(&request.declared_project) {
        if previous_root != &external_root {
            return Err(cuenv_core::Error::configuration(format!(
                "cross-project reference '{}' is ambiguous: it resolves to both '{}' and '{}'",
                request.declared_project,
                previous_root.display(),
                external_root.display()
            )));
        }
        return Ok(ExternalProjectOutcome::AlreadyLoaded);
    }

    let external_package = cue_package_name(&external_root)?;
    let external_executor = CommandExecutor::new(external_package.clone());
    let mut external_manifest = super::super::discovery::evaluate_manifest(
        &external_root,
        &external_package,
        &external_executor,
    )?;
    let external_index = list_builder::prepare_task_index(&mut external_manifest, &external_root)?;

    // Record the roots only once the project has evaluated, so a failed load
    // leaves no half-registered alias behind.
    if !external_manifest.name.is_empty() {
        project_roots.insert(external_manifest.name.clone(), external_root.clone());
    }
    project_roots.insert(request.declared_project.clone(), external_root.clone());

    Ok(ExternalProjectOutcome::Loaded {
        root: external_root,
        tasks: external_index.to_tasks(),
    })
}

fn project_roots_from_module(
    module: &cuenv_core::module::ModuleEvaluation,
) -> BTreeMap<String, PathBuf> {
    let mut roots = BTreeMap::new();
    for instance in module.projects() {
        let absolute = module.root.join(&instance.path);
        if let Some(name) = instance.project_name() {
            roots.insert(name.to_string(), absolute.clone());
        }
        let relative = instance.path.to_string_lossy().replace('\\', "/");
        if !relative.is_empty() {
            roots.insert(relative, absolute);
        }
    }
    roots
}

fn collect_project_requests(tasks: &Tasks, origin_root: &Path) -> Vec<ExternalProjectRequest> {
    let mut requests = BTreeSet::new();
    for node in tasks.tasks.values() {
        collect_node_project_requests(node, origin_root, &mut requests);
    }
    requests.into_iter().collect()
}

fn collect_node_project_requests(
    node: &TaskNode,
    origin_root: &Path,
    requests: &mut BTreeSet<ExternalProjectRequest>,
) {
    match node {
        TaskNode::Task(task) => {
            requests.extend(
                task.iter_project_refs()
                    .map(|reference| ExternalProjectRequest {
                        declared_project: reference.project.clone(),
                        origin_root: origin_root.to_path_buf(),
                    }),
            );
        }
        TaskNode::Group(group) => {
            for child in group.children.values() {
                collect_node_project_requests(child, origin_root, requests);
            }
        }
        TaskNode::Sequence(steps) => {
            for step in steps {
                collect_node_project_requests(step, origin_root, requests);
            }
        }
    }
}

fn resolve_external_project_root(
    project_roots: &BTreeMap<String, PathBuf>,
    request: &ExternalProjectRequest,
) -> Result<PathBuf> {
    if let Some(root) = project_roots.get(&request.declared_project) {
        return std::fs::canonicalize(root).map_err(|error| {
            cuenv_core::Error::io_with_path("canonicalize referenced project", root.clone(), error)
        });
    }

    let declared = Path::new(&request.declared_project);
    let workspace_root = git_hooks::find_git_root(&request.origin_root)
        .ok()
        .or_else(|| {
            request
                .origin_root
                .ancestors()
                .find(|ancestor| ancestor.join(".git").exists())
                .map(Path::to_path_buf)
        })
        .or_else(|| crate::commands::env_file::find_cue_module_root(&request.origin_root))
        .unwrap_or_else(|| request.origin_root.clone());
    let candidate = if declared.is_absolute() {
        let workspace_relative = declared
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(part) => Some(part),
                std::path::Component::ParentDir => Some(std::ffi::OsStr::new("..")),
                std::path::Component::Prefix(_)
                | std::path::Component::RootDir
                | std::path::Component::CurDir => None,
            })
            .collect::<PathBuf>();
        workspace_root.join(workspace_relative)
    } else {
        request.origin_root.join(declared)
    };
    let canonical_workspace = std::fs::canonicalize(&workspace_root).map_err(|error| {
        cuenv_core::Error::io_with_path(
            "canonicalize cross-project workspace",
            workspace_root.clone(),
            error,
        )
    })?;
    let canonical_project = std::fs::canonicalize(&candidate).map_err(|error| {
        cuenv_core::Error::io_with_path("canonicalize referenced project", candidate, error)
    })?;
    if !canonical_project.starts_with(&canonical_workspace) {
        return Err(cuenv_core::Error::configuration(format!(
            "cross-project reference '{}' resolves outside workspace '{}'",
            request.declared_project,
            canonical_workspace.display()
        )));
    }
    Ok(canonical_project)
}

fn cue_package_name(project_root: &Path) -> Result<String> {
    let env_file = project_root.join("env.cue");
    let contents = std::fs::read_to_string(&env_file).map_err(|error| {
        cuenv_core::Error::io_with_path("read referenced project package", env_file.clone(), error)
    })?;
    contents
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("package ").map(str::trim))
        .filter(|package| !package.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "referenced project '{}' has no package declaration in env.cue",
                project_root.display()
            ))
        })
}

fn qualify_external_task_node(
    node: TaskNode,
    project_alias: &str,
    project_root: &Path,
) -> TaskNode {
    match node {
        TaskNode::Task(mut task) => {
            for dependency in &mut task.depends_on {
                if !dependency.name.starts_with('#') {
                    dependency.name = format!("#{project_alias}:{}", dependency.name);
                }
            }
            task.project_root = Some(project_root.to_path_buf());
            TaskNode::Task(task)
        }
        TaskNode::Group(mut group) => {
            for dependency in &mut group.depends_on {
                if !dependency.name.starts_with('#') {
                    dependency.name = format!("#{project_alias}:{}", dependency.name);
                }
            }
            group.children = group
                .children
                .into_iter()
                .map(|(name, child)| {
                    (
                        name,
                        qualify_external_task_node(child, project_alias, project_root),
                    )
                })
                .collect();
            TaskNode::Group(group)
        }
        TaskNode::Sequence(steps) => TaskNode::Sequence(
            steps
                .into_iter()
                .map(|step| qualify_external_task_node(step, project_alias, project_root))
                .collect(),
        ),
    }
}

/// Internal implementation of task execution.
pub(in crate::commands::task) async fn execute_task_impl(
    request: &TaskExecutionRequest<'_>,
) -> Result<String> {
    let input = TaskExecutionInput::from_request(request);

    // Handle CLI help immediately if no task specified
    if input.should_show_general_help() {
        return Ok(rendering::get_task_cli_help());
    }

    tracing::info!(
        "Executing task from path: {}, package: {}, task: {:?}",
        input.path,
        input.package,
        input.task_name
    );

    let context = load_task_execution_context(&input)?;

    if let Some(output) = picker::maybe_run_interactive_picker(&input, &context).await? {
        return Ok(output);
    }

    if let Some(output) = listing::maybe_render_task_list(&input, &context)? {
        return Ok(output);
    }

    let normalized_labels = selection::validate_task_selection(&input)?;
    if let Some(output) = help::maybe_render_task_help(&input, &context)? {
        return Ok(output);
    }
    let resolution = selection::resolve_task_selection(&input, &context, &normalized_labels)?;

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
    let requested = resolution.display_name.as_str();
    // The filters are checked here, once, and not again for each task.
    if let Some(env) = &context.manifest.env {
        cuenv_core::context::validate(env, &context.manifest.contexts)?;
    }
    let mut environments = ProjectTaskEnvironments {
        runtime_env: base_runtime_environment(context).await?,
        env: context.manifest.env.clone(),
        definitions: context.manifest.contexts.clone(),
        environment: input.environment.map(str::to_string),
        project_root: context.project_root.clone(),
        activation_steps: lockfile_activation_steps(&context.manifest, &context.project_root)
            .await?,
        shared: EnvironmentKey::default(),
        resolved: Mutex::default(),
    };
    let contexts = if let TaskNode::Task(task) = &resolution.node {
        environments.contexts_for(TaskSubject::of(requested, task))?
    } else {
        // A group or sequence is not a task, so no task filter is active for
        // it, whatever its name: its tasks are matched one at a time.
        let mut contexts = environments.contexts_for(TaskSubject::named(requested))?;
        contexts.retain(|name| {
            !matches!(
                environments.definitions.get(name),
                Some(ContextDefinition::Task(_))
            )
        });
        contexts
    };
    environments.shared = environments.key_for(requested, contexts);
    let env = environments.build(requested, &environments.shared).await?;
    let selects_by_task = environments.env.is_some();

    Ok(PreparedTaskRuntime {
        env,
        task_environments: selects_by_task
            .then(|| Arc::new(environments) as Arc<dyn TaskEnvironments>),
        cache: task_cache_for_context(context).await,
    })
}

async fn base_runtime_environment(context: &TaskExecutionContext) -> Result<Environment> {
    let mut runtime_env = Environment::new();
    let runtime_env_vars = cuenv_core::runtime::resolve_runtime_environment(
        &context.project_root,
        context.manifest.runtime.as_ref(),
    )
    .await?;
    for (key, value) in runtime_env_vars {
        runtime_env.set(key, value);
    }
    Ok(runtime_env)
}

/// An environment that is built once, by whichever task needs it first.
type BuiltEnvironment = Arc<OnceCell<Environment>>;

/// What decides the project variables a task receives. Two tasks with the
/// same key share an environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
struct EnvironmentKey {
    /// The contexts active for the task, sorted.
    contexts: Vec<String>,
    /// The selected variables whose policies allow the task, sorted.
    variables: Vec<String>,
}

/// Builds the environment of each task of a run.
///
/// A named context can select variables for one task and not for another,
/// and a variable's policies can allow one task and not another, so the
/// tasks of one graph do not all share an environment. The environment of
/// the requested task is built up front; any other is built the first time a
/// task needs it, so a secret no task of the graph may receive is never
/// resolved.
#[derive(Debug)]
struct ProjectTaskEnvironments {
    /// The runtime's environment, before the project's variables.
    runtime_env: Environment,
    env: Option<Env>,
    /// The project's `contexts`, which `prepare_task_runtime` has validated.
    definitions: HashMap<String, ContextDefinition>,
    /// The selected environment (`-e`).
    environment: Option<String>,
    project_root: PathBuf,
    activation_steps: Vec<ResolvedToolActivationStep>,
    /// The key of the requested task, whose environment every task runs
    /// with unless its own key differs.
    shared: EnvironmentKey,
    /// The environments built so far. Each is built once; building one does
    /// not hold up a task that needs another.
    resolved: Mutex<HashMap<EnvironmentKey, BuiltEnvironment>>,
}

impl ProjectTaskEnvironments {
    /// The contexts active for a task, sorted. No context is reported, and
    /// no fact detected, unless `env.context` sets something.
    fn contexts_for(&self, task: TaskSubject<'_>) -> Result<Vec<String>> {
        if !self.env.as_ref().is_some_and(Env::has_context_variables) {
            return Ok(Vec::new());
        }
        cuenv_core::context::active_contexts(
            &self.definitions,
            &Facts::detect(&self.project_root),
            Consumer::Task(task),
        )
    }

    /// The project variables selected while `contexts` are active.
    fn selected(&self, contexts: &[String]) -> HashMap<String, EnvValue> {
        self.env.as_ref().map_or_else(HashMap::new, |env| {
            env.select(EnvSelection {
                environment: self.environment.as_deref(),
                contexts,
            })
        })
    }

    /// The key of the task called `name` while `contexts` are active.
    fn key_for(&self, name: &str, contexts: Vec<String>) -> EnvironmentKey {
        let mut variables: Vec<String> = self
            .selected(&contexts)
            .into_iter()
            .filter(|(_, value)| value.is_accessible_by_task(name))
            .map(|(variable, _)| variable)
            .collect();
        variables.sort();
        EnvironmentKey {
            contexts,
            variables,
        }
    }

    /// The environment for a key, resolved on behalf of the task called
    /// `name`: every task with that key may receive the same variables.
    async fn build(&self, name: &str, key: &EnvironmentKey) -> Result<Environment> {
        let mut environment = self.runtime_env.clone();

        let env_vars = self.selected(&key.contexts);
        let (task_env_vars, secrets) =
            Environment::resolve_for_task_with_secrets(name, &env_vars).await?;

        cuenv_events::register_secrets(secrets);

        for (variable, value) in task_env_vars {
            // Resolution flattens everything to a plain string, so the CUE
            // declaration is the last place that still knows which values are
            // credentials. Carry that through: a secret must be fingerprinted
            // rather than written into the cache key.
            if env_vars.get(&variable).is_some_and(EnvValue::is_secret) {
                environment.set_secret(variable, value);
            } else {
                environment.set(variable, value);
            }
        }

        for step in &self.activation_steps {
            let current = environment.get(&step.var);
            if let Some(new_value) =
                cuenv_tool_runtime::apply_resolved_tool_activation(current, step)
            {
                environment.set(step.var.clone(), new_value);
            }
        }
        Ok(environment)
    }
}

impl TaskEnvironments for ProjectTaskEnvironments {
    fn environment_for<'a>(&'a self, name: &'a str, task: &'a Task) -> TaskEnvironmentFuture<'a> {
        Box::pin(async move {
            let contexts = self.contexts_for(TaskSubject::of(name, task))?;
            let key = self.key_for(name, contexts);
            if key == self.shared {
                return Ok(None);
            }
            let cell = Arc::clone(self.resolved.lock().await.entry(key.clone()).or_default());
            let environment = cell.get_or_try_init(|| self.build(name, &key)).await?;
            Ok(Some(environment.clone()))
        })
    }
}

/// The lockfile's tool activation steps, after downloading the tools; none
/// unless the project uses the tools runtime.
async fn lockfile_activation_steps(
    manifest: &Project,
    project_root: &Path,
) -> Result<Vec<ResolvedToolActivationStep>> {
    if !should_activate_lockfile_tools(manifest) {
        return Ok(Vec::new());
    }
    crate::commands::tools::ensure_tools_downloaded(Some(project_root))
        .await
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to download tools: {e}")))?;
    let activation_steps =
        crate::commands::tools::resolve_tool_activation_steps(Some(project_root))
            .map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to resolve tools activation: {e}"))
            })?
            .unwrap_or_default();
    tracing::debug!(
        steps = activation_steps.len(),
        "Applying configured tool activation operations for task execution"
    );
    Ok(activation_steps)
}

fn should_activate_lockfile_tools(project: &Project) -> bool {
    matches!(project.runtime, Some(Runtime::Tools(_)))
}

async fn task_cache_for_context(context: &TaskExecutionContext) -> Option<TaskCacheConfig> {
    let module_root = context
        .cue_module_root
        .as_deref()
        .unwrap_or(context.project_root.as_path());
    let hasher_root = task_hasher_root(context);
    let runtime_identity = super::super::cache::resolve_runtime_cache_identity(
        module_root,
        context.project_root.as_path(),
        context.manifest.runtime.as_ref(),
    );
    if let Some(reason) = &runtime_identity.cache_disabled_reason {
        tracing::warn!(reason, "task cache disabled for this invocation");
    }
    super::super::cache::build_task_cache(
        TaskCacheContext {
            project_root: &context.project_root,
            hasher_root: &hasher_root,
            project_roots: context.project_roots.clone(),
            runtime_identity,
        },
        context.manifest.cache.as_ref(),
    )
    .await
}

fn task_hasher_root(context: &TaskExecutionContext) -> PathBuf {
    let module_root = context
        .cue_module_root
        .as_deref()
        .unwrap_or(context.project_root.as_path());
    git_hooks::find_git_root(&context.project_root).unwrap_or_else(|_| {
        context
            .project_root
            .ancestors()
            .find(|ancestor| ancestor.join(".git").exists())
            .map_or_else(|| module_root.to_path_buf(), Path::to_path_buf)
    })
}

struct TaskRunRequest<'execution, 'input> {
    input: &'execution TaskExecutionInput<'input>,
    context: &'execution TaskExecutionContext,
    resolution: &'execution TaskResolution,
    task_graph: &'execution TaskGraph,
    runtime: PreparedTaskRuntime,
}

async fn execute_resolved_task(request: TaskRunRequest<'_, '_>) -> Result<String> {
    let executor_config = task_executor_config(&TaskExecutorConfigSpec {
        input: request.input,
        context: request.context,
        runtime: &request.runtime,
        capture_output: request.input.capture_output,
    });
    let executor = TaskExecutor::new(executor_config);

    if request.input.uses_tui() && request.task_graph.task_count() > 0 {
        let tui_config = task_executor_config(&TaskExecutorConfigSpec {
            input: request.input,
            context: request.context,
            runtime: &request.runtime,
            capture_output: OutputCapture::Capture,
        });
        let tui_executor = TaskExecutor::new(tui_config);

        return runner::execute_with_rich_tui(
            &tui_executor,
            request.resolution.display_name.as_str(),
            request.task_graph,
        )
        .await;
    }

    let results = runner::execute_task_with_strategy(StrategyInput {
        executor: &executor,
        task_name: request.resolution.display_name.as_str(),
        task_node: &request.resolution.node,
        task_graph: request.task_graph,
        all_tasks: &request.resolution.tasks,
    })
    .await?;

    if let Some(failed) = results.iter().find(|result| !result.success) {
        return Err(cuenv_core::Error::configuration(
            cuenv_task_exec::executor::summarize_task_failure(failed, TASK_FAILURE_SNIPPET_LINES),
        ));
    }

    Ok(runner::format_task_results(
        results,
        request.input.capture_output,
        request.resolution.display_name.as_str(),
    ))
}

struct TaskExecutorConfigSpec<'execution, 'input> {
    input: &'execution TaskExecutionInput<'input>,
    context: &'execution TaskExecutionContext,
    runtime: &'execution PreparedTaskRuntime,
    capture_output: OutputCapture,
}

fn task_executor_config(spec: &TaskExecutorConfigSpec<'_, '_>) -> ExecutorConfig {
    ExecutorConfig {
        capture_output: spec.capture_output,
        max_parallel: 0,
        continue_on_error: spec.input.continue_on_error,
        environment: spec.runtime.env.clone(),
        task_environments: spec.runtime.task_environments.clone(),
        working_dir: None,
        cue_module_root: spec.context.cue_module_root.clone(),
        project_root: spec.context.project_root.clone(),
        cache: spec.runtime.cache.clone(),
        sandbox_hasher_root: Some(task_hasher_root(spec.context)),
        sandbox_project_roots: spec.context.project_roots.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn environments(
        env: serde_json::Value,
        contexts: serde_json::Value,
    ) -> ProjectTaskEnvironments {
        ProjectTaskEnvironments {
            runtime_env: Environment::new(),
            env: Some(serde_json::from_value(env).unwrap()),
            definitions: serde_json::from_value(contexts).unwrap(),
            environment: None,
            project_root: std::env::temp_dir(),
            activation_steps: Vec::new(),
            shared: EnvironmentKey::default(),
            resolved: Mutex::default(),
        }
    }

    fn task(command: &str) -> Task {
        Task {
            command: command.to_string(),
            ..Default::default()
        }
    }

    /// The variables a task of the graph runs with, when `requested` is the
    /// task that was asked for.
    async fn variables_of(
        mut environments: ProjectTaskEnvironments,
        requested: &str,
        name: &str,
    ) -> Vec<String> {
        let contexts = environments
            .contexts_for(TaskSubject::of(requested, &task("true")))
            .unwrap();
        environments.shared = environments.key_for(requested, contexts);
        let shared = environments
            .build(requested, &environments.shared)
            .await
            .unwrap();
        let environment = environments
            .environment_for(name, &task("true"))
            .await
            .unwrap()
            .unwrap_or(shared);
        let mut variables: Vec<String> = environment.iter().map(|(key, _)| key.clone()).collect();
        variables.sort();
        variables
    }

    #[tokio::test]
    async fn a_policy_is_checked_for_the_task_that_receives_the_variable() {
        let env = json!({
            "PLAIN": "x",
            "FOR_BUILD": {"value": "b", "policies": [{"allowTasks": ["build"]}]},
            "FOR_DEPLOY": {"value": "d", "policies": [{"allowTasks": ["deploy"]}]},
        });
        // `deploy` was asked for and `build` is one of its dependencies.
        assert_eq!(
            variables_of(environments(env.clone(), json!({})), "deploy", "build").await,
            ["FOR_BUILD", "PLAIN"]
        );
        assert_eq!(
            variables_of(environments(env, json!({})), "deploy", "deploy").await,
            ["FOR_DEPLOY", "PLAIN"]
        );
    }

    #[tokio::test]
    async fn a_context_variable_reaches_the_task_its_policy_allows() {
        let env = json!({
            "context": {"build": {
                "TOKEN": {"value": "t", "policies": [{"allowTasks": ["build"]}]},
            }},
        });
        let contexts = json!({"build": {"name": "build"}});
        assert_eq!(
            variables_of(
                environments(env.clone(), contexts.clone()),
                "deploy",
                "build"
            )
            .await,
            ["TOKEN"]
        );
        assert!(
            variables_of(environments(env, contexts), "deploy", "deploy")
                .await
                .is_empty()
        );
    }
}
