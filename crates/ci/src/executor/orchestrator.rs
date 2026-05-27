//! CI Pipeline Orchestrator
//!
//! Main entry point for CI pipeline execution, integrating with the provider
//! system for context detection, file change tracking, and reporting.
//!
//! This module owns project-level scheduling and reporting. Per-task DAG
//! execution lives in `task_execution`.

use crate::affected::compute_affected_tasks;
use crate::discovery::evaluate_module_from_cwd;
use crate::provider::CIProvider;
use crate::report::{ContextReport, PipelineReport, PipelineStatus};
use chrono::{DateTime, Utc};
use cuenv_core::manifest::Project;
use cuenv_core::tasks::TaskIndex;
use cuenv_core::{DryRun, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::hook_env::build_hook_environment;
use super::reporting::{
    cache_policy_override_for, notify_provider, register_ci_secrets, resolve_annotations,
    write_pipeline_report,
};
use super::task_env::resolve_environment;
use super::task_execution::{PipelineTaskResults, PipelineTasksRequest, execute_pipeline_tasks};

/// Request for running CI pipelines from the executor.
pub struct RunCiRequest {
    /// CI provider used for context, changed files, and reporting.
    pub provider: Arc<dyn CIProvider>,
    /// Whether to skip actual task execution.
    pub dry_run: DryRun,
    /// Optional pipeline name to run.
    pub specific_pipeline: Option<String>,
    /// Optional environment override for secrets resolution.
    pub environment: Option<String>,
    /// Optional module-root-relative project path filter.
    pub path_filter: Option<String>,
    /// Maximum parallel jobs inside each task DAG walk.
    pub max_parallel: usize,
}

/// Run the CI pipeline logic
///
/// This is the main entry point for CI execution, integrating with the provider
/// system for context detection, file change tracking, and reporting.
///
/// # Errors
/// Returns error if IO errors occur or tasks fail
pub async fn run_ci(request: RunCiRequest) -> Result<()> {
    let RunCiRequest {
        provider,
        dry_run,
        specific_pipeline,
        environment,
        path_filter,
        max_parallel,
    } = request;

    let context = provider.context();
    cuenv_events::emit_ci_context!(&context.provider, &context.event, &context.ref_name);

    // Get changed files
    let changed_files = provider.changed_files().await?;
    cuenv_events::emit_ci_changed_files!(changed_files.len());

    let Some(discovered) = load_ci_projects(path_filter.as_deref())? else {
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
        max_parallel: max_parallel.max(1),
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
    max_parallel: usize,
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
        max_parallel,
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
                max_parallel,
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
    pub max_parallel: usize,
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
    let max_parallel = request.max_parallel;

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
        max_parallel,
    })
    .await;

    let completed_at = Utc::now();
    let duration_ms = pipeline_duration_ms(start_time, completed_at);

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

fn pipeline_duration_ms(started_at: DateTime<Utc>, completed_at: DateTime<Utc>) -> u64 {
    u64::try_from((completed_at - started_at).num_milliseconds()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn pipeline_duration_ms_returns_elapsed_milliseconds() {
        let started_at = Utc::now();
        let completed_at = started_at + Duration::milliseconds(1_250);

        assert_eq!(pipeline_duration_ms(started_at, completed_at), 1_250);
    }

    #[test]
    fn pipeline_duration_ms_clamps_negative_durations() {
        let completed_at = Utc::now();
        let started_at = completed_at + Duration::milliseconds(10);

        assert_eq!(pipeline_duration_ms(started_at, completed_at), 0);
    }
}
