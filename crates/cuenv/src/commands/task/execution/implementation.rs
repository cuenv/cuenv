//! Task execution orchestration.

use super::super::cache::TaskCacheContext;
use super::super::{dag_export, projects};

use super::super::runner::StrategyInput;
use super::super::types::TaskExecutionRequest;
use super::super::{list_builder, rendering, runner};

use cuenv_core::environment::Environment;
use cuenv_core::manifest::Project;

use cuenv_core::tasks::TaskNode;
use cuenv_core::{OutputCapture, Result};
use cuenv_task_exec::cache::TaskCacheConfig;
use cuenv_task_exec::executor::TASK_FAILURE_SNIPPET_LINES;
use cuenv_task_exec::{
    ExecutorConfig, ProjectExecution, TaskEnvironments, TaskExecutor, TaskGraph, TaskScope,
};

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use super::context::{TaskExecutionContext, TaskExecutionInput};
use super::environments::{
    ProjectEnvironmentSpec, ProjectTaskEnvironments, RunEnvironments, RunTaskEnvironments,
};
use super::selection::TaskResolution;
use super::{help, listing, picker, selection};

struct PreparedTaskRuntime {
    env: Environment,
    /// Environments for the tasks that do not run with `env`: those a
    /// context selects other variables for, and those of other projects.
    task_environments: Option<Arc<dyn TaskEnvironments>>,
    cache: Option<TaskCacheConfig>,
    /// What the tasks of other projects run with, by project.
    external_projects: BTreeMap<String, ProjectExecution>,
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
    let root = resolution.graph_root_name.as_str();
    tracing::debug!("Building task graph for task: {root}");

    let task_graph = if input.skip_dependencies {
        tracing::debug!("Skipping dependencies - adding only the target task");
        let mut task_graph = TaskGraph::new();
        if let Some(TaskNode::Task(task)) = resolution.scope.local().get(root) {
            task_graph.add_task(root, (**task).clone())?;
        }
        task_graph.add_output_ref_deps(resolution.scope.output_ref_deps(), &resolution.scope)?;
        task_graph
    } else {
        resolution.scope.graph([root]).map_err(|e| {
            tracing::error!("Failed to build task graph: {}", e);
            e
        })?
    };

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
    let own = ProjectTaskEnvironments::prepare(ProjectEnvironmentSpec {
        manifest: &context.manifest,
        project_root: &context.project_root,
        environment: input.environment,
    })
    .await?;
    let requested_env = match &resolution.node {
        TaskNode::Task(task) => own.task_env(requested, task)?,
        _ => own.group_env(requested)?,
    };
    let env = own.environment(requested_env.as_ref()).await?;

    let environments = RunTaskEnvironments::new(RunEnvironments {
        own,
        shared: requested_env
            .as_ref()
            .map(|env| env.key().clone())
            .unwrap_or_default(),
        scope: &resolution.scope,
        environment: input.environment,
    });
    let cache = task_cache_for_context(context).await;

    Ok(PreparedTaskRuntime {
        env,
        external_projects: external_project_executions(&resolution.scope, cache.as_ref()),
        task_environments: environments
            .differs_by_task()
            .then(|| Arc::new(environments) as Arc<dyn TaskEnvironments>),
        cache,
    })
}

async fn task_cache_for_context(context: &TaskExecutionContext) -> Option<TaskCacheConfig> {
    let module_root = context
        .cue_module_root
        .as_deref()
        .unwrap_or(context.project_root.as_path());
    let hasher_root = projects::workspace_root(&context.project_root);
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
            runtime_identity,
        },
        context.manifest.cache.as_ref(),
    )
    .await
}

/// Where each task of another project runs, and what its results are keyed
/// with: its own project's directory, CUE module and runtime identity.
fn external_project_executions(
    scope: &TaskScope,
    cache: Option<&TaskCacheConfig>,
) -> BTreeMap<String, ProjectExecution> {
    scope
        .external_projects()
        .map(|(key, project)| {
            let cache = cache.map(|cache| {
                let identity = super::super::cache::resolve_runtime_cache_identity(
                    &project.module_root,
                    &project.root,
                    project.manifest.runtime.as_ref(),
                );
                if let Some(reason) = &identity.cache_disabled_reason {
                    tracing::warn!(project = key, reason, "task cache disabled for a project");
                }
                super::super::cache::with_runtime_identity(cache, identity)
            });
            (
                key.to_string(),
                ProjectExecution {
                    project_root: project.root.clone(),
                    cue_module_root: Some(project.module_root.clone()),
                    cache,
                },
            )
        })
        .collect()
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

    // A group or sequence runs its own children. What they depend on
    // outside it runs first, as it would for a single task.
    let prerequisites = if request.input.skip_dependencies {
        None
    } else {
        runner::prerequisites_outside(
            &request.resolution.node,
            request.resolution.graph_root_name.as_str(),
            &request.resolution.scope,
        )?
    };

    let results = runner::execute_task_with_strategy(StrategyInput {
        executor: &executor,
        task_name: request.resolution.display_name.as_str(),
        task_node: &request.resolution.node,
        task_graph: request.task_graph,
        prerequisites: prerequisites.as_ref(),
        all_tasks: request.resolution.scope.local(),
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
        sandbox_hasher_root: Some(projects::workspace_root(&spec.context.project_root)),
        external_projects: spec.runtime.external_projects.clone(),
    }
}
