//! CI Pipeline Orchestrator
//!
//! Main entry point for CI pipeline execution, integrating with the provider
//! system for context detection, file change tracking, and reporting.
//!
//! This module orchestrates complex async workflows with caching, concurrency control,
//! and multi-project coordination. The complexity is inherent to the domain.

use crate::affected::{compute_affected_tasks, matched_inputs_for_task};
use crate::compiler::Compiler;
use crate::discovery::evaluate_module_from_cwd;
use crate::ir::CachePolicy;
use crate::provider::CIProvider;
use crate::report::{ContextReport, PipelineReport, PipelineStatus, TaskReport, TaskStatus};
use chrono::Utc;
use cuenv_core::manifest::Project;
use cuenv_core::tasks::captures::resolve_captures;
use cuenv_core::tasks::{TaskGraph, TaskIndex};
use cuenv_core::{DryRun, Result};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::ExecutorError;
use super::hook_env::build_hook_environment;
use super::reporting::{
    cache_policy_override_for, notify_provider, register_ci_secrets, resolve_annotations,
    write_pipeline_report,
};
use super::runner::{IRTaskRunner, TaskOutput};
use super::tools::{
    apply_tool_activation_steps, ensure_tools_downloaded, resolve_tool_activation_steps,
};

/// Run the CI pipeline logic
///
/// This is the main entry point for CI execution, integrating with the provider
/// system for context detection, file change tracking, and reporting.
///
/// # Arguments
///
/// * `provider` - The CI provider to use for changed files detection and reporting
/// * `dry_run` - Whether to skip actual task execution
/// * `specific_pipeline` - If set, only run tasks from this pipeline
/// * `environment` - Optional environment override for secrets resolution
/// * `path_filter` - If set, only process projects under this path (relative to module root)
///
/// # Errors
/// Returns error if IO errors occur or tasks fail
pub async fn run_ci(
    provider: Arc<dyn CIProvider>,
    dry_run: DryRun,
    specific_pipeline: Option<String>,
    environment: Option<String>,
    path_filter: Option<&str>,
) -> Result<()> {
    let context = provider.context();
    cuenv_events::emit_ci_context!(&context.provider, &context.event, &context.ref_name);

    // Get changed files
    let changed_files = provider.changed_files().await?;
    cuenv_events::emit_ci_changed_files!(changed_files.len());

    let Some(discovered) = load_ci_projects(path_filter)? else {
        return Ok(());
    };

    let failures = run_ci_projects(CiProjectRunRequest {
        provider: provider.as_ref(),
        dry_run,
        specific_pipeline: specific_pipeline.as_deref(),
        environment: environment.as_deref(),
        context,
        changed_files: &changed_files,
        discovered: &discovered,
    })
    .await?;

    if !failures.is_empty() {
        return Err(cuenv_core::Error::execution(format!(
            "CI pipeline failed:\n\n{}",
            format_pipeline_failures(&failures)
        )));
    }

    Ok(())
}

struct DiscoveredCiProjects {
    projects: Vec<(PathBuf, Project)>,
    project_map: ProjectDependencyMap,
    project_configs: ProjectConfigMap,
}

type ProjectDependencyMap = HashMap<String, (PathBuf, Project)>;
type ProjectConfigMap = HashMap<PathBuf, Project>;

fn load_ci_projects(path_filter: Option<&str>) -> Result<Option<DiscoveredCiProjects>> {
    // Evaluate module and discover projects
    let module = evaluate_module_from_cwd()?;
    let project_count = module.project_count();
    if project_count == 0 {
        tracing::info!("No cuenv projects discovered; skipping CI run.");
        return Ok(None);
    }
    cuenv_events::emit_ci_projects_discovered!(project_count);

    // Collect projects with their configs
    let mut projects: Vec<(PathBuf, Project)> = Vec::new();
    for instance in module.projects() {
        let config = Project::try_from(instance)?;
        let project_path = module.root.join(&instance.path);
        projects.push((project_path, config));
    }

    // Filter projects by path if specified (and not the default ".")
    let projects: Vec<(PathBuf, Project)> = match path_filter {
        Some(filter) if filter != "." => {
            let filter_path = module.root.join(filter);
            projects
                .into_iter()
                .filter(|(path, _)| path.starts_with(&filter_path))
                .collect()
        }
        _ => projects,
    };

    if projects.is_empty() {
        tracing::info!(
            filter = path_filter.unwrap_or("."),
            "No projects under path filter; skipping"
        );
        return Ok(None);
    }

    let (project_map, project_configs) = build_project_lookup(&projects);

    Ok(Some(DiscoveredCiProjects {
        projects,
        project_map,
        project_configs,
    }))
}

fn build_project_lookup(
    projects: &[(PathBuf, Project)],
) -> (ProjectDependencyMap, ProjectConfigMap) {
    let mut project_map = HashMap::new();
    let mut project_configs = HashMap::new();
    for (path, config) in projects {
        let name = config.name.trim();
        if !name.is_empty() {
            project_map.insert(name.to_string(), (path.clone(), config.clone()));
        }

        project_configs.insert(path.clone(), config.clone());
        if let Ok(canonical) = path.canonicalize() {
            project_configs.insert(canonical, config.clone());
        }
    }

    (project_map, project_configs)
}

struct CiProjectRunRequest<'a> {
    provider: &'a dyn CIProvider,
    dry_run: DryRun,
    specific_pipeline: Option<&'a str>,
    environment: Option<&'a str>,
    context: &'a crate::context::CIContext,
    changed_files: &'a [PathBuf],
    discovered: &'a DiscoveredCiProjects,
}

async fn run_ci_projects(
    request: CiProjectRunRequest<'_>,
) -> Result<Vec<(String, cuenv_core::Error)>> {
    let CiProjectRunRequest {
        provider,
        dry_run,
        specific_pipeline,
        environment,
        context,
        changed_files,
        discovered,
    } = request;

    // Track failures with structured errors
    let mut failures: Vec<(String, cuenv_core::Error)> = Vec::new();

    // Process each project
    for (project_path, config) in &discovered.projects {
        let Some(plan) = plan_project_pipeline(ProjectPipelinePlanRequest {
            project_path,
            config,
            requested_pipeline: specific_pipeline,
            requested_environment: environment,
            context,
            changed_files,
            project_map: &discovered.project_map,
        })?
        else {
            continue;
        };

        tracing::info!(
            project = %project_path.display(),
            tasks = ?plan.tasks_to_run,
            "Running tasks for project"
        );

        if !dry_run.is_dry_run() {
            let result = execute_project_pipeline(&PipelineExecutionRequest {
                project_path,
                config,
                pipeline_name: &plan.pipeline_name,
                tasks_to_run: &plan.tasks_to_run,
                environment: plan.environment.as_deref(),
                context,
                changed_files,
                provider,
                project_configs: &discovered.project_configs,
            })
            .await;

            match result {
                Err(e) => {
                    tracing::error!(error = %e, "Pipeline execution error");
                    let project_name = project_path.display().to_string();
                    failures.push((project_name, e));
                }
                Ok((status, task_errors)) => {
                    if status == PipelineStatus::Failed {
                        failures.extend(task_errors);
                    }
                }
            }
        }
    }

    Ok(failures)
}

#[derive(Clone, Copy)]
struct ProjectPipelinePlanRequest<'a> {
    project_path: &'a Path,
    config: &'a Project,
    requested_pipeline: Option<&'a str>,
    requested_environment: Option<&'a str>,
    context: &'a crate::context::CIContext,
    changed_files: &'a [PathBuf],
    project_map: &'a ProjectDependencyMap,
}

struct ProjectPipelinePlan {
    pipeline_name: String,
    tasks_to_run: Vec<String>,
    environment: Option<String>,
}

fn plan_project_pipeline(
    request: ProjectPipelinePlanRequest<'_>,
) -> Result<Option<ProjectPipelinePlan>> {
    let ProjectPipelinePlanRequest {
        project_path,
        config,
        requested_pipeline,
        requested_environment,
        context,
        changed_files,
        project_map,
    } = request;

    let pipeline_name = requested_pipeline.unwrap_or("default").to_string();
    let Some(ci) = &config.ci else {
        return Err(cuenv_core::Error::configuration(format!(
            "Project {} has no CI configuration",
            project_path.display()
        )));
    };

    let available_pipelines: Vec<&str> = ci.pipelines.keys().map(String::as_str).collect();
    let Some(pipeline) = ci.pipelines.get(&pipeline_name) else {
        return Err(cuenv_core::Error::configuration(format!(
            "Pipeline '{}' not found in project {}. Available pipelines: {}",
            pipeline_name,
            project_path.display(),
            available_pipelines.join(", ")
        )));
    };

    let environment = resolve_environment(requested_environment, pipeline.environment.as_deref());
    let pipeline_task_names: Vec<String> = pipeline
        .tasks
        .iter()
        .map(|task| task.task_name().to_string())
        .collect();

    let tasks_to_run = if context.event == "release" {
        pipeline_task_names
    } else {
        compute_affected_tasks(
            changed_files,
            &pipeline_task_names,
            project_path,
            config,
            project_map,
        )
    };

    if tasks_to_run.is_empty() {
        cuenv_events::emit_ci_project_skipped!(project_path.display(), "No affected tasks");
        return Ok(None);
    }

    Ok(Some(ProjectPipelinePlan {
        pipeline_name,
        tasks_to_run,
        environment,
    }))
}

fn format_pipeline_failures(failures: &[(String, cuenv_core::Error)]) -> String {
    failures
        .iter()
        .map(|(project, err)| format!("  [{project}]\n    {err}"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// All parameters needed to execute a project pipeline.
pub struct PipelineExecutionRequest<'a> {
    pub project_path: &'a Path,
    pub config: &'a Project,
    pub pipeline_name: &'a str,
    pub tasks_to_run: &'a [String],
    pub environment: Option<&'a str>,
    pub context: &'a crate::context::CIContext,
    pub changed_files: &'a [PathBuf],
    pub provider: &'a dyn CIProvider,
    pub project_configs: &'a HashMap<PathBuf, Project>,
}

/// Execute a project's pipeline and handle reporting
///
/// Returns the pipeline status and a list of task failures (project path, error).
async fn execute_project_pipeline(
    request: &PipelineExecutionRequest<'_>,
) -> Result<(PipelineStatus, Vec<(String, cuenv_core::Error)>)> {
    let project_path = request.project_path;
    let config = request.config;
    let pipeline_name = request.pipeline_name;
    let tasks_to_run = request.tasks_to_run;
    let environment = request.environment;
    let context = request.context;
    let changed_files = request.changed_files;
    let provider = request.provider;
    let project_configs = request.project_configs;

    let start_time = Utc::now();
    let project_display = project_path.display().to_string();
    let cache_policy_override = cache_policy_override_for(context);

    // Register common CI secret patterns for redaction.
    // These are typically passed via GitHub Actions secrets or similar.
    register_ci_secrets();

    // Merge static + hook-generated environment once per project, then reuse for all tasks.
    let hook_env = build_hook_environment(project_path, config, project_configs).await?;

    // Build task index for resolving nested task names (e.g., "deploy.preview")
    let task_index = TaskIndex::build(&config.tasks)?;

    let PipelineTaskResults {
        reports: tasks_reports,
        status: pipeline_status,
        errors: task_errors,
        captures: all_captures,
    } = execute_pipeline_tasks(PipelineTasksRequest {
        project_path,
        project_display: &project_display,
        config,
        task_names: tasks_to_run,
        environment,
        changed_files,
        task_index: &task_index,
        cache_policy_override,
        hook_env: &hook_env,
        continue_on_error: pipeline_continue_on_error(config, pipeline_name),
    })
    .await;

    let completed_at = Utc::now();
    #[allow(clippy::cast_sign_loss)]
    let duration_ms = (completed_at - start_time).num_milliseconds() as u64;

    // Resolve pipeline annotations from capture refs
    let Some(ci) = &config.ci else {
        unreachable!("CI config already validated above");
    };
    let resolved_annotations = ci
        .pipelines
        .get(pipeline_name)
        .map(|p| resolve_annotations(&p.annotations, &all_captures))
        .unwrap_or_default();

    // Generate report
    let report = PipelineReport {
        version: cuenv_core::VERSION.to_string(),
        project: project_path.display().to_string(),
        pipeline: pipeline_name.to_string(),
        context: ContextReport {
            provider: context.provider.clone(),
            event: context.event.clone(),
            ref_name: context.ref_name.clone(),
            base_ref: context.base_ref.clone(),
            sha: context.sha.clone(),
            changed_files: changed_files
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect(),
        },
        started_at: start_time,
        completed_at: Some(completed_at),
        duration_ms: Some(duration_ms),
        status: pipeline_status,
        tasks: tasks_reports,
        annotations: resolved_annotations,
    };

    // Write reports and notify provider
    write_pipeline_report(&report, context, project_path);
    notify_provider(provider, &report, pipeline_name).await;

    Ok((pipeline_status, task_errors))
}

fn pipeline_continue_on_error(config: &Project, pipeline_name: &str) -> bool {
    config
        .ci
        .as_ref()
        .and_then(|ci| ci.pipelines.get(pipeline_name))
        .is_some_and(|pipeline| pipeline.continue_on_error)
}

#[derive(Clone, Copy)]
struct PipelineTasksRequest<'a> {
    project_path: &'a Path,
    project_display: &'a str,
    config: &'a Project,
    task_names: &'a [String],
    environment: Option<&'a str>,
    changed_files: &'a [PathBuf],
    task_index: &'a TaskIndex,
    cache_policy_override: Option<CachePolicy>,
    hook_env: &'a BTreeMap<String, String>,
    continue_on_error: bool,
}

struct PipelineTaskResults {
    reports: Vec<TaskReport>,
    status: PipelineStatus,
    errors: Vec<(String, cuenv_core::Error)>,
    captures: HashMap<String, HashMap<String, String>>,
}

impl PipelineTaskResults {
    fn new() -> Self {
        Self {
            reports: Vec::new(),
            status: PipelineStatus::Success,
            errors: Vec::new(),
            captures: HashMap::new(),
        }
    }

    fn record(&mut self, project_display: &str, task_name: &str, outcome: PipelineTaskOutcome) {
        if !outcome.captures.is_empty() {
            self.captures
                .insert(task_name.to_string(), outcome.captures.clone());
        }

        if let Some(error) = outcome.error {
            self.status = PipelineStatus::Failed;
            self.errors.push((project_display.to_string(), error));
        }

        self.reports.push(outcome.report);
    }
}

struct PipelineTaskOutcome {
    report: TaskReport,
    captures: HashMap<String, String>,
    error: Option<cuenv_core::Error>,
}

async fn execute_pipeline_tasks(request: PipelineTasksRequest<'_>) -> PipelineTaskResults {
    let mut results = PipelineTaskResults::new();
    for task_name in request.task_names {
        let outcome = execute_pipeline_task(PipelineTaskRequest {
            task_name,
            pipeline: request,
        })
        .await;
        results.record(request.project_display, task_name, outcome);
    }
    results
}

#[derive(Clone, Copy)]
struct PipelineTaskRequest<'a> {
    task_name: &'a str,
    pipeline: PipelineTasksRequest<'a>,
}

async fn execute_pipeline_task(request: PipelineTaskRequest<'_>) -> PipelineTaskOutcome {
    let PipelineTaskRequest {
        task_name,
        pipeline,
    } = request;
    let inputs_matched = matched_inputs_for_task(
        task_name,
        pipeline.config,
        pipeline.changed_files,
        pipeline.project_path,
    );
    let outputs = task_outputs(pipeline.task_index, task_name);

    cuenv_events::emit_ci_task_executing!(pipeline.project_display, task_name);
    let task_start = std::time::Instant::now();

    let result = execute_task_with_deps(TaskDagOptions {
        config: pipeline.config,
        task_name,
        project_root: pipeline.project_path,
        cache_policy_override: pipeline.cache_policy_override,
        environment: pipeline.environment,
        hook_env: pipeline.hook_env,
        continue_on_error: pipeline.continue_on_error,
    })
    .await;

    let duration_ms = u64::try_from(task_start.elapsed().as_millis()).unwrap_or(0);
    match result {
        Ok(output) => task_outcome_from_output(TaskOutputOutcomeRequest {
            task_name,
            project_display: pipeline.project_display,
            task_index: pipeline.task_index,
            output,
            inputs_matched,
            outputs,
            duration_ms,
        }),
        Err(error) => {
            tracing::error!(error = %error, task = task_name, "Task execution error");
            cuenv_events::emit_ci_task_result!(pipeline.project_display, task_name, false);
            PipelineTaskOutcome {
                report: TaskReport {
                    name: task_name.to_string(),
                    status: TaskStatus::Failed,
                    duration_ms,
                    exit_code: None,
                    cache_key: None,
                    inputs_matched,
                    outputs,
                    captures: HashMap::new(),
                },
                captures: HashMap::new(),
                error: Some(error.into()),
            }
        }
    }
}

struct TaskOutputOutcomeRequest<'a> {
    task_name: &'a str,
    project_display: &'a str,
    task_index: &'a TaskIndex,
    output: TaskOutput,
    inputs_matched: Vec<String>,
    outputs: Vec<String>,
    duration_ms: u64,
}

fn task_outcome_from_output(request: TaskOutputOutcomeRequest<'_>) -> PipelineTaskOutcome {
    let TaskOutputOutcomeRequest {
        task_name,
        project_display,
        task_index,
        output,
        inputs_matched,
        outputs,
        duration_ms,
    } = request;
    let captures = task_captures(task_index, task_name, &output);

    if output.success {
        cuenv_events::emit_ci_task_result!(project_display, task_name, true);
        return PipelineTaskOutcome {
            report: TaskReport {
                name: task_name.to_string(),
                status: TaskStatus::Success,
                duration_ms,
                exit_code: Some(output.exit_code),
                cache_key: Some(task_cache_key(&output)),
                inputs_matched,
                outputs,
                captures: captures.clone(),
            },
            captures,
            error: None,
        };
    }

    cuenv_events::emit_ci_task_result!(project_display, task_name, false);
    let error =
        cuenv_core::Error::task_failed(task_name, output.exit_code, &output.stdout, &output.stderr);
    PipelineTaskOutcome {
        report: TaskReport {
            name: task_name.to_string(),
            status: TaskStatus::Failed,
            duration_ms,
            exit_code: Some(output.exit_code),
            cache_key: None,
            inputs_matched,
            outputs,
            captures: captures.clone(),
        },
        captures,
        error: Some(error),
    }
}

fn task_outputs(task_index: &TaskIndex, task_name: &str) -> Vec<String> {
    task_index
        .resolve(task_name)
        .ok()
        .and_then(|indexed| indexed.node.as_task())
        .map(|task| task.outputs.clone())
        .unwrap_or_default()
}

fn task_captures(
    task_index: &TaskIndex,
    task_name: &str,
    output: &TaskOutput,
) -> HashMap<String, String> {
    task_index
        .resolve(task_name)
        .ok()
        .and_then(|indexed| indexed.node.as_task())
        .filter(|task| !task.captures.is_empty())
        .map(|task| resolve_captures(&task.captures, &output.stdout, &output.stderr))
        .unwrap_or_default()
}

fn task_cache_key(output: &TaskOutput) -> String {
    if output.from_cache {
        format!("cached:{}", output.task_id)
    } else {
        output.task_id.clone()
    }
}

/// Apply task-level environment variables to the merged task environment.
///
/// Task env has the highest precedence. Passthrough placeholders intentionally
/// read from the host process at execution time so CI-provided values such as
/// GitHub Actions context variables remain available inside hermetic tasks.
fn apply_task_env(env: &mut BTreeMap<String, String>, task_env: &BTreeMap<String, String>) {
    for (key, value) in task_env {
        if let Some(host_var) = cuenv_core::tasks::output_refs::parse_passthrough(value) {
            match std::env::var(host_var) {
                Ok(host_value) => {
                    env.insert(key.clone(), host_value);
                }
                Err(_) => {
                    env.remove(key);
                }
            }
        } else if !value.starts_with("cuenv:ref:") {
            env.insert(key.clone(), value.clone());
        }
    }
}

fn resolve_environment(
    cli_environment: Option<&str>,
    pipeline_environment: Option<&str>,
) -> Option<String> {
    if let Some(env) = cli_environment.filter(|name| !name.is_empty()) {
        return Some(env.to_string());
    }

    if let Ok(env) = std::env::var("CUENV_ENVIRONMENT")
        && !env.is_empty()
    {
        return Some(env);
    }

    pipeline_environment
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string())
}

/// Default in-project parallelism cap for the CI orchestrator.
///
/// Each project's task DAG is executed level-by-level (dependency-respecting);
/// within a level we run up to this many tasks concurrently. Matches the
/// `max_parallel` default used by the core executor.
const CI_MAX_PARALLEL: usize = 4;

/// Execute a task with all its dependencies in correct order.
///
/// Per-call options for [`execute_task_with_deps`].
struct TaskDagOptions<'a> {
    config: &'a Project,
    task_name: &'a str,
    project_root: &'a Path,
    cache_policy_override: Option<CachePolicy>,
    environment: Option<&'a str>,
    hook_env: &'a BTreeMap<String, String>,
    continue_on_error: bool,
}

/// Uses TaskIndex to flatten nested tasks and TaskGraph to resolve dependencies,
/// then walks the graph by topological "parallel groups" so independent tasks
/// at the same dependency depth run concurrently (bounded by [`CI_MAX_PARALLEL`]).
///
/// Behaviour:
/// - When `continue_on_error` is `false` (default), the first failure aborts
///   the rest of the run and returns the failing task's `TaskOutput`.
/// - When `continue_on_error` is `true`, dependents of a failing task are
///   marked as `Skipped { DependencyFailed }` (and `TaskEvent::Skipped` is
///   emitted), but unrelated sibling chains continue executing. A `JoinError`
///   (task panic / spawn failure) is always fatal regardless of the flag —
///   we can't reason about state after a panic.
///
/// The returned `TaskOutput` is the originally-requested task's output —
/// resolved by matching the canonical name once execution finishes.
async fn execute_task_with_deps(
    opts: TaskDagOptions<'_>,
) -> std::result::Result<TaskOutput, ExecutorError> {
    use cuenv_core::tasks::graph_walk::{WalkPolicy, walk_parallel_graph};

    let TaskDagOptions {
        config,
        task_name,
        project_root,
        cache_policy_override,
        environment,
        hook_env,
        continue_on_error,
    } = opts;

    let index =
        TaskIndex::build(&config.tasks).map_err(|e| ExecutorError::Compilation(e.to_string()))?;
    let entry = index
        .resolve(task_name)
        .map_err(|e| ExecutorError::Compilation(e.to_string()))?;
    let canonical_name = entry.name.clone();
    let flattened_tasks = index.to_tasks();

    let mut graph = TaskGraph::new();
    graph
        .build_for_task(&canonical_name, &flattened_tasks)
        .map_err(|e| ExecutorError::Compilation(e.to_string()))?;

    let parallel_groups = graph
        .get_parallel_groups()
        .map_err(|e| ExecutorError::Compilation(e.to_string()))?;

    tracing::info!(
        task = task_name,
        canonical = %canonical_name,
        continue_on_error,
        execution_order = ?parallel_groups
            .iter()
            .map(|g| g.iter().map(|n| n.name.clone()).collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        "Resolved task dependencies"
    );
    drop(parallel_groups);

    let config = Arc::new(config.clone());
    let project_root = Arc::new(project_root.to_path_buf());
    let environment = environment.map(str::to_string);
    let hook_env = Arc::new(hook_env.clone());

    let policy = WalkPolicy {
        max_parallel: CI_MAX_PARALLEL,
        continue_on_error,
    };
    let summary = walk_parallel_graph(
        graph.inner(),
        policy,
        // CI doesn't have cross-task output refs that need resolving
        // before spawn — the IR compiler handles its own substitutions
        // inside the per-task path. Pass through unchanged.
        cuenv_core::tasks::graph_walk::passthrough_prepare::<_, _, ExecutorError>,
        {
            let config = Arc::clone(&config);
            let project_root = Arc::clone(&project_root);
            let hook_env = Arc::clone(&hook_env);
            move |node: cuenv_task_graph::GraphNode<cuenv_core::tasks::Task>| {
                let config = Arc::clone(&config);
                let project_root = Arc::clone(&project_root);
                let environment = environment.clone();
                let hook_env = Arc::clone(&hook_env);
                async move {
                    compile_and_execute_ir(
                        config.as_ref(),
                        &node.name,
                        project_root.as_ref(),
                        cache_policy_override,
                        environment.as_deref(),
                        hook_env.as_ref(),
                    )
                    .await
                }
            }
        },
        |err: tokio::task::JoinError| {
            ExecutorError::Compilation(format!("CI orchestrator panic: {err}"))
        },
    )
    .await?;

    let mut outputs: HashMap<String, TaskOutput> = summary.outcomes.into_iter().collect();

    // If any task failed, surface its TaskOutput so the caller can record
    // a non-success status for the originally-requested task. Under
    // continue_on_error we still want to return non-Ok behaviour up
    // through the pipeline-level success bookkeeping.
    if summary.failed > 0
        && let Some(failed) = outputs.values().find(|o| !o.success).cloned()
    {
        return Ok(failed);
    }

    outputs
        .remove(&canonical_name)
        .ok_or_else(|| ExecutorError::Compilation("No tasks to execute".into()))
}

/// Compile a single task to IR and execute it.
///
/// This is the inner execution loop - it does NOT handle dependencies.
/// Dependencies are resolved by the outer loop using TaskGraph.
/// Uses the Compiler to convert task definitions to IR.
async fn compile_and_execute_ir(
    config: &Project,
    task_name: &str,
    project_root: &Path,
    cache_policy_override: Option<CachePolicy>,
    environment: Option<&str>,
    hook_env: &BTreeMap<String, String>,
) -> std::result::Result<TaskOutput, ExecutorError> {
    let start = std::time::Instant::now();

    // Use the Compiler to compile the task (handles both single tasks and groups)
    let options = crate::compiler::CompilerOptions {
        project_root: Some(project_root.to_path_buf()),
        ..Default::default()
    };
    let compiler = Compiler::with_options(config.clone(), options);
    let ir = compiler
        .compile_task(task_name)
        .map_err(|e| ExecutorError::Compilation(e.to_string()))?;

    if ir.tasks.is_empty() {
        return Err(ExecutorError::Compilation(format!(
            "Task '{task_name}' produced no executable tasks"
        )));
    }

    // Resolve secrets from project environment (same as CLI).
    // Prefer an explicit environment name, then fall back to CUENV_ENVIRONMENT.
    let env_name = environment
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("CUENV_ENVIRONMENT")
                .ok()
                .filter(|name| !name.is_empty())
        });
    let project_env_vars = config
        .env
        .as_ref()
        .map(|env| match env_name.as_deref() {
            Some(name) => env.for_environment(name),
            None => env.base.clone(),
        })
        .unwrap_or_default();
    let (resolved_env, secrets) =
        cuenv_core::environment::Environment::resolve_for_task_with_secrets(
            task_name,
            &project_env_vars,
        )
        .await
        .map_err(|e| ExecutorError::Compilation(format!("Secret resolution failed: {e}")))?;

    // Register resolved secrets for redaction
    cuenv_events::register_secrets(secrets);

    // Execute all compiled IR tasks sequentially
    let runner = IRTaskRunner::new(
        project_root.to_path_buf(),
        cuenv_core::OutputCapture::Capture,
    );
    let mut combined_stdout = String::new();
    let mut combined_stderr = String::new();
    let mut all_success = true;
    let mut last_exit_code = 0;

    // Resolve runtime environment (devenv/nix) if configured on the project.
    // This runs `devenv print-dev-env` or `nix print-dev-env` and captures the
    // resulting environment variables (PATH, etc.) so tasks execute within the
    // correct runtime.
    let runtime_env =
        cuenv_core::runtime::resolve_runtime_environment(project_root, config.runtime.as_ref())
            .await
            .map_err(|e| ExecutorError::Compilation(e.to_string()))?;
    if !runtime_env.is_empty() {
        tracing::info!(
            vars = runtime_env.len(),
            "Resolved runtime environment for CI task execution"
        );
    }

    // Ensure tools are downloaded before getting activation paths.
    // Tool activation failures are fatal.
    ensure_tools_downloaded(project_root).await?;
    let activation_steps = resolve_tool_activation_steps(project_root)?;
    if !activation_steps.is_empty() {
        tracing::debug!(
            steps = activation_steps.len(),
            "Applying configured tool activation operations for CI task execution"
        );
    }

    for ir_task in &ir.tasks {
        // Build environment with explicit precedence:
        // task env > resolved project env > runtime env > hook/static env.
        let mut env: BTreeMap<String, String> = hook_env.clone();
        for (key, value) in &runtime_env {
            env.insert(key.clone(), value.clone());
        }
        for (key, value) in &resolved_env {
            env.insert(key.clone(), value.clone());
        }
        apply_task_env(&mut env, &ir_task.env);

        apply_tool_activation_steps(&mut env, &activation_steps);

        // Ensure the running cuenv binary is on PATH so tasks that invoke
        // `cuenv` (e.g., ci.sync-check) can find it even when hooks replace
        // PATH entirely (e.g., NixFlake).
        if let Ok(exe) = std::env::current_exe()
            && let Some(exe_dir) = exe.parent()
        {
            let exe_dir_str = exe_dir.to_string_lossy();
            if let Some(existing_path) = env.get("PATH") {
                if !existing_path.contains(exe_dir_str.as_ref()) {
                    env.insert("PATH".to_string(), format!("{exe_dir_str}:{existing_path}"));
                }
            } else {
                env.insert("PATH".to_string(), exe_dir_str.to_string());
            }
        }

        if !env.contains_key("HOME")
            && let Ok(home) = std::env::var("HOME")
        {
            env.insert("HOME".to_string(), home);
        }

        // Apply cache policy override if specified
        let mut task_to_run = ir_task.clone();
        if let Some(policy) = cache_policy_override {
            task_to_run.cache_policy = policy;
        }

        let output = runner.execute(&task_to_run, env).await?;

        combined_stdout.push_str(&output.stdout);
        combined_stderr.push_str(&output.stderr);
        last_exit_code = output.exit_code;

        if !output.success {
            all_success = false;
            break; // Stop on first failure
        }
    }

    let duration = start.elapsed();
    let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);

    Ok(TaskOutput {
        task_id: task_name.to_string(),
        exit_code: last_exit_code,
        stdout: combined_stdout,
        stderr: combined_stderr,
        success: all_success,
        from_cache: false,
        duration_ms,
        captures: HashMap::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::apply_task_env;
    use std::collections::BTreeMap;

    #[test]
    fn task_env_passthrough_reads_host_environment() {
        temp_env::with_var("CUENV_TEST_GITHUB_ACTOR", Some("octocat"), || {
            let mut env = BTreeMap::from([("GITHUB_ACTOR".to_string(), "lower".to_string())]);
            let task_env = BTreeMap::from([(
                "GITHUB_ACTOR".to_string(),
                "cuenv:passthrough:CUENV_TEST_GITHUB_ACTOR".to_string(),
            )]);

            apply_task_env(&mut env, &task_env);

            assert_eq!(env.get("GITHUB_ACTOR"), Some(&"octocat".to_string()));
        });
    }

    #[test]
    fn missing_task_env_passthrough_unsets_lower_precedence_value() {
        temp_env::with_var_unset("CUENV_TEST_MISSING_ACTOR", || {
            let mut env = BTreeMap::from([("GITHUB_ACTOR".to_string(), "lower".to_string())]);
            let task_env = BTreeMap::from([(
                "GITHUB_ACTOR".to_string(),
                "cuenv:passthrough:CUENV_TEST_MISSING_ACTOR".to_string(),
            )]);

            apply_task_env(&mut env, &task_env);

            assert!(!env.contains_key("GITHUB_ACTOR"));
        });
    }

    #[test]
    fn literal_task_env_overrides_lower_precedence_value() {
        let mut env = BTreeMap::from([("GITHUB_REF_NAME".to_string(), "main".to_string())]);
        let task_env = BTreeMap::from([("GITHUB_REF_NAME".to_string(), "release".to_string())]);

        apply_task_env(&mut env, &task_env);

        assert_eq!(env.get("GITHUB_REF_NAME"), Some(&"release".to_string()));
    }
}
