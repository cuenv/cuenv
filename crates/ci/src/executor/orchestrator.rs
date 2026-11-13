//! CI Pipeline Orchestrator
//!
//! Main entry point for CI pipeline execution, integrating with the provider
//! system for context detection, file change tracking, and reporting.
//!
//! Tasks are left out of a run only when a comparison with the run's base
//! shows them to be unaffected. A run whose provider has no base runs every
//! task of the pipeline and says why.
//!
//! This module owns project-level scheduling and reporting. Per-task DAG
//! execution lives in `task_execution`.

use crate::affected::{
    AffectedProjects, ChangedInputs, CheckoutAnchors, CheckoutFrame, PipelineReach,
};
use crate::configuration::{ConfigurationSources, ProjectInstance};
use crate::discovery;
use crate::provider::{CIProvider, ChangedFiles};
use crate::report::{ContextReport, PipelineReport, PipelineStatus};
use chrono::{DateTime, Utc};
use cuenv_core::ci::PipelineTask;
use cuenv_core::manifest::Project;
use cuenv_core::tasks::file_references::ReferenceChecks;
use cuenv_core::{DryRun, Result};
use cuenv_task_exec::TaskIndex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::projects::{CiModule, CiProjects, HookProject};
use super::task_env;
use super::task_execution::{PipelineScope, PipelineTaskResults, PipelineTasksRequest};

/// Which tasks to select within the requested project and pipeline scope.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TaskSelection {
    /// Select tasks affected by the provider's changed files, or every task
    /// of the pipeline when the provider has no base to compare with.
    #[default]
    Affected,
    /// Select every declared named pipeline task.
    All,
}

/// Request for running CI pipelines from the executor.
pub struct RunCiRequest {
    /// CI provider used for context, changed files, and reporting.
    pub provider: Arc<dyn CIProvider>,
    /// Whether to skip actual task execution.
    pub dry_run: DryRun,
    /// Whether task selection uses changed files or all declared tasks.
    pub task_selection: TaskSelection,
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
        task_selection,
        specific_pipeline,
        environment,
        path_filter,
        max_parallel,
    } = request;

    let context = provider.context();
    cuenv_events::emit_ci_context!(&context.provider, &context.event, &context.ref_name);

    // Compare the run with its base. A run without one cannot show any task
    // to be unaffected, and says so where the user reads the run.
    let changes = provider.changed_files().await?;
    match &changes {
        ChangedFiles::Compared(files) => cuenv_events::emit_ci_changed_files!(files.len()),
        ChangedFiles::NoBase(why) => cuenv_events::emit_stdout!(why),
    }

    let Some(discovered) = load_ci_projects(path_filter.as_deref())? else {
        return match task_selection {
            TaskSelection::Affected => Ok(()),
            TaskSelection::All => Err(cuenv_core::Error::configuration(
                "No CI projects match the requested --all scope",
            )),
        };
    };

    let failures = run_ci_projects(CiProjectRunRequest {
        provider: provider.as_ref(),
        dry_run,
        task_selection,
        specific_pipeline: specific_pipeline.as_deref(),
        environment: environment.as_deref(),
        context,
        changes: &changes,
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
    /// The projects in scope: every project, or those under `--path`.
    projects: Vec<(PathBuf, Project)>,
    /// Every project of the module by name, in or out of scope: a task in
    /// scope may depend on a task of a project that `--path` leaves out.
    /// Each project's directory is relative to the root of the checkout,
    /// the frame changed files are reported in.
    project_map: ProjectDependencyMap,
    /// Where the checkout's root is.
    frame: CheckoutFrame,
    /// The root of the CUE module.
    module_root: PathBuf,
    /// Every project directory of the module, in or out of scope.
    project_directories: Vec<PathBuf>,
    /// Every project of the module, for running those dependencies.
    all_projects: Arc<CiProjects>,
    project_configs: ProjectConfigMap,
    /// The directory `--path` names, when it names one.
    named_path: Option<PathBuf>,
}

type ProjectDependencyMap = HashMap<String, (PathBuf, Project)>;
type ProjectConfigMap = HashMap<PathBuf, Project>;

fn load_ci_projects(path_filter: Option<&str>) -> Result<Option<DiscoveredCiProjects>> {
    // Evaluate module and discover projects
    let module = discovery::evaluate_module_from_cwd()?;
    let project_count = module.project_count();
    if project_count == 0 {
        tracing::info!("No cuenv projects discovered; skipping CI run.");
        return Ok(None);
    }
    cuenv_events::emit_ci_projects_discovered!(project_count);

    // Collect projects with their configs, in a stable order: the module
    // keeps instances in a hash map, and neither the order projects run in
    // nor which directory a project name resolves to may depend on it.
    let mut projects: Vec<(PathBuf, Project)> = Vec::new();
    for instance in module.projects() {
        let config = instance.to_project()?;
        let project_path = module.root.join(&instance.path);
        projects.push((project_path, config));
    }
    projects.sort_by(|(left, _), (right, _)| left.cmp(right));

    // Dependencies are resolved against every project of the module, before
    // `--path` narrows what runs.
    let frame = CheckoutFrame::around(&module.root);
    let named_projects = build_project_map(&projects);
    // A dependency runs in the project it is selected by: one name rule and
    // one checkout root serve both.
    let all_projects = Arc::new(CiProjects::new(CiModule {
        module_root: &module.root,
        checkout_root: frame.root(),
        projects: &projects,
        named: &named_projects,
    }));
    let project_map = place_projects(named_projects, &frame);
    let project_directories = projects.iter().map(|(path, _)| path.clone()).collect();

    // Filter projects by path if specified (and not the default ".")
    let named_path = path_filter
        .filter(|filter| *filter != ".")
        .map(|filter| module.root.join(filter));
    let projects: Vec<(PathBuf, Project)> = match &named_path {
        Some(filter_path) => projects
            .into_iter()
            .filter(|(path, _)| path.starts_with(filter_path))
            .collect(),
        None => projects,
    };

    if projects.is_empty() {
        tracing::info!(
            filter = path_filter.unwrap_or("."),
            "No projects under path filter; skipping"
        );
        return Ok(None);
    }

    let project_configs = build_project_configs(&projects);

    Ok(Some(DiscoveredCiProjects {
        projects,
        project_map,
        frame,
        module_root: module.root.clone(),
        project_directories,
        all_projects,
        project_configs,
        named_path,
    }))
}

/// Every named project by its name.
///
/// A directory below a project, in the same package, inherits the project's
/// `name` and is part of that project, so one name can be carried by several
/// directories. The name means the directory that declares it: the
/// shallowest one. Project names are otherwise unique in a module.
fn build_project_map(projects: &[(PathBuf, Project)]) -> ProjectDependencyMap {
    let mut project_map: ProjectDependencyMap = HashMap::new();
    for (path, config) in projects {
        let name = config.name.trim();
        if name.is_empty() {
            continue;
        }
        let declares_name = project_map
            .get(name)
            .is_none_or(|(known, _)| path.components().count() < known.components().count());
        if declares_name {
            project_map.insert(name.to_string(), (path.clone(), config.clone()));
        }
    }
    project_map
}

/// Give every project its directory relative to the root of the checkout.
///
/// A project outside the checkout cannot be compared with changed files. It
/// is left out, so a dependency on it does not resolve and counts as
/// affected.
fn place_projects(projects: ProjectDependencyMap, frame: &CheckoutFrame) -> ProjectDependencyMap {
    projects
        .into_iter()
        .filter_map(|(name, (path, config))| {
            let placed = frame.place(&path);
            if placed.is_none() {
                cuenv_events::emit_stderr!(format!(
                    "warning: project '{name}' at {} is outside the checkout at {}, so its \
                     files cannot be compared with changes; tasks that depend on it always run",
                    path.display(),
                    frame.root().display()
                ));
            }
            placed.map(|placed| (name, (placed, config)))
        })
        .collect()
}

fn build_project_configs(projects: &[(PathBuf, Project)]) -> ProjectConfigMap {
    let mut project_configs = HashMap::new();
    for (path, config) in projects {
        project_configs.insert(path.clone(), config.clone());
        if let Ok(canonical) = path.canonicalize() {
            project_configs.insert(canonical, config.clone());
        }
    }
    project_configs
}

struct CiProjectRunRequest<'request> {
    provider: &'request dyn CIProvider,
    dry_run: DryRun,
    task_selection: TaskSelection,
    specific_pipeline: Option<&'request str>,
    environment: Option<&'request str>,
    context: &'request crate::context::CIContext,
    changes: &'request ChangedFiles,
    discovered: &'request DiscoveredCiProjects,
    max_parallel: usize,
}

async fn run_ci_projects(
    request: CiProjectRunRequest<'_>,
) -> Result<Vec<(String, cuenv_core::Error)>> {
    let CiProjectRunRequest {
        provider,
        dry_run,
        task_selection,
        specific_pipeline,
        environment,
        context,
        changes,
        discovered,
        max_parallel,
    } = request;

    // Track failures with structured errors
    let mut failures: Vec<(String, cuenv_core::Error)> = Vec::new();

    // Plan every project first, so secret providers can prepare for the
    // whole run before any task executes.
    // A run with no base compared nothing, so no configuration is known to
    // have changed; it selects every task anyway.
    let changed_configuration = changed_configurations(discovered, changes.files());
    let module_prefix = discovered.frame.place(&discovered.module_root);

    let mut plans = Vec::new();
    let mut pipeline_defined = false;
    for (project_path, config) in &discovered.projects {
        let scope = if discovered.named_path.as_deref() == Some(project_path.as_path()) {
            ProjectScope::Named
        } else {
            ProjectScope::Discovered
        };
        match plan_project_pipeline(ProjectPipelinePlanRequest {
            project_path,
            config,
            scope,
            task_selection,
            requested_pipeline: specific_pipeline,
            requested_environment: environment,
            changes,
            project_map: &discovered.project_map,
            frame: &discovered.frame,
            module_prefix: module_prefix.as_deref(),
            projects: &discovered.all_projects,
            changed_configuration: &changed_configuration,
        })? {
            ProjectPlan::Run(plan) => {
                pipeline_defined = true;
                plans.push((project_path, config, plan));
            }
            ProjectPlan::NothingAffected => pipeline_defined = true,
            ProjectPlan::NoSuchPipeline => {}
        }
    }
    crate::affected::fail_on_root_relative_inputs().map_err(|failures| {
        cuenv_core::Error::configuration(format!(
            "affected detection cannot compare these inputs with the changed files:\n{failures}"
        ))
    })?;

    if !pipeline_defined {
        return Err(no_project_defines_pipeline(
            specific_pipeline.unwrap_or(DEFAULT_PIPELINE),
            &discovered.projects,
        ));
    }

    if !dry_run.is_dry_run() {
        prepare_secret_providers(&plans, &discovered.all_projects).await?;
    }

    for (project_path, config, plan) in plans {
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
                changes,
                provider,
                project_configs: &discovered.project_configs,
                all_projects: &discovered.all_projects,
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

/// Let secret providers prepare for every secret the planned pipelines can
/// resolve.
///
/// Tasks resolve their environment when they start, so without this a
/// provider would first log in when the first task that needs it runs,
/// possibly long after a short-lived credential (such as the GitHub Actions
/// OIDC token behind Infisical OIDC Auth) has expired.
async fn prepare_secret_providers(
    plans: &[(&PathBuf, &Project, ProjectPipelinePlan)],
    all_projects: &CiProjects,
) -> Result<()> {
    let mut env_vars = HashMap::new();
    for (project_path, config, plan) in plans {
        // The tasks about to run, and the tasks of other projects they
        // depend on, each resolve their own project's secrets.
        let values =
            super::task_execution::pipeline_env_values(super::task_execution::PlannedPipeline {
                project_path,
                config,
                task_names: &plan.tasks_to_run,
                environment: plan.environment.as_deref(),
                projects: all_projects,
            });
        for (index, value) in values.into_iter().enumerate() {
            // Keys only need to be unique; secrets are grouped by resolver.
            env_vars.insert(format!("{}:{index}", project_path.display()), value);
        }
    }
    cuenv_core::environment::Environment::prepare_secret_providers(&env_vars).await
}

/// The pipeline `cuenv ci` runs when none is named.
const DEFAULT_PIPELINE: &str = "default";

/// How a project came to be in the scope of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectScope {
    /// `--path` names this project's directory, so the caller asked for it.
    Named,
    /// The project was discovered: by a run without `--path`, or beneath the
    /// directory `--path` names.
    Discovered,
}

/// What planning one project's pipeline came to.
#[derive(Debug)]
enum ProjectPlan {
    /// The project has the pipeline and these tasks run.
    Run(ProjectPipelinePlan),
    /// The project has the pipeline and no task of it is affected.
    NothingAffected,
    /// A discovered project has no CI configuration, or not this pipeline.
    NoSuchPipeline,
}

/// The error for a run in which no project in scope defines the pipeline.
///
/// A run without `--path` skips projects that do not define the requested
/// pipeline, so a mistyped pipeline name would otherwise run nothing and
/// succeed.
fn no_project_defines_pipeline(
    pipeline_name: &str,
    projects: &[(PathBuf, Project)],
) -> cuenv_core::Error {
    let mut available: Vec<&str> = projects
        .iter()
        .filter_map(|(_, config)| config.ci.as_ref())
        .flat_map(|ci| ci.pipelines.keys().map(String::as_str))
        .collect();
    available.sort_unstable();
    available.dedup();
    if available.is_empty() {
        return cuenv_core::Error::configuration(format!(
            "No project in scope defines pipeline '{pipeline_name}': none of them has a CI \
             configuration"
        ));
    }
    cuenv_core::Error::configuration(format!(
        "No project in scope defines pipeline '{pipeline_name}'. Pipelines that exist: {}",
        available.join(", ")
    ))
}

#[derive(Clone, Copy)]
struct ProjectPipelinePlanRequest<'request> {
    project_path: &'request Path,
    config: &'request Project,
    scope: ProjectScope,
    task_selection: TaskSelection,
    requested_pipeline: Option<&'request str>,
    requested_environment: Option<&'request str>,
    changes: &'request ChangedFiles,
    project_map: &'request ProjectDependencyMap,
    frame: &'request CheckoutFrame,
    /// The root of the CUE module relative to the root of the checkout,
    /// where `schema.#ModuleFiles` paths start; `None` outside it.
    module_prefix: Option<&'request Path>,
    /// Every project of the module, which binds file references through
    /// what each project imports.
    projects: &'request CiProjects,
    /// The checkout-relative directories of the projects whose CUE
    /// configuration a changed file can change.
    changed_configuration: &'request HashSet<PathBuf>,
}

/// The projects of the module whose CUE configuration a changed file can
/// change, by their directory relative to the root of the checkout.
///
/// A project whose sources cannot be listed, or that is not inside the
/// checkout, cannot be shown to be unchanged: the first is reported and
/// counts as changed, the second is handled where the project is planned.
fn changed_configurations(
    discovered: &DiscoveredCiProjects,
    changed_files: &[PathBuf],
) -> HashSet<PathBuf> {
    // Nothing changed, so no configuration did.
    if changed_files.is_empty() {
        return HashSet::new();
    }
    let Some(module_prefix) = discovered.frame.place(&discovered.module_root) else {
        return HashSet::new();
    };

    discovered
        .project_directories
        .iter()
        .filter_map(|directory| {
            let placed = discovered.frame.place(directory)?;
            let changed = match ConfigurationSources::load(ProjectInstance {
                module_root: &discovered.module_root,
                directory,
                module_prefix: &module_prefix,
            }) {
                Ok(sources) => sources.changed_by(changed_files),
                Err(error) => {
                    cuenv_events::emit_stderr!(format!(
                        "warning: {error}; its configuration cannot be shown to be unchanged, \
                         so every task of the project is selected"
                    ));
                    true
                }
            };
            changed.then_some(placed)
        })
        .collect()
}

#[derive(Debug)]
struct ProjectPipelinePlan {
    pipeline_name: String,
    tasks_to_run: Vec<String>,
    environment: Option<String>,
}

fn plan_project_pipeline(request: ProjectPipelinePlanRequest<'_>) -> Result<ProjectPlan> {
    let ProjectPipelinePlanRequest {
        project_path,
        config,
        scope,
        requested_pipeline,
        requested_environment,
        task_selection,
        changes,
        project_map,
        frame,
        module_prefix,
        projects,
        changed_configuration,
    } = request;

    // A project the caller named must have the pipeline. A discovered one
    // may not: a module's projects need not all define every pipeline, and a
    // run over the whole module (Buildkite's generated step, a root
    // project's thin workflow) skips those that do not.
    let pipeline_name = requested_pipeline.unwrap_or(DEFAULT_PIPELINE).to_string();
    let Some(ci) = &config.ci else {
        if scope == ProjectScope::Discovered {
            tracing::debug!(
                project = %project_path.display(),
                "Project has no CI configuration; skipping"
            );
            return Ok(ProjectPlan::NoSuchPipeline);
        }
        return Err(cuenv_core::Error::configuration(format!(
            "Project {} has no CI configuration",
            project_path.display()
        )));
    };

    let Some(pipeline) = ci.pipelines.get(&pipeline_name) else {
        if scope == ProjectScope::Discovered {
            tracing::debug!(
                project = %project_path.display(),
                pipeline = %pipeline_name,
                "Project does not define the pipeline; skipping"
            );
            return Ok(ProjectPlan::NoSuchPipeline);
        }
        let available_pipelines: Vec<&str> = ci.pipelines.keys().map(String::as_str).collect();
        return Err(cuenv_core::Error::configuration(format!(
            "Pipeline '{}' not found in project {}. Available pipelines: {}",
            pipeline_name,
            project_path.display(),
            available_pipelines.join(", ")
        )));
    };

    let environment =
        task_env::resolve_environment(requested_environment, pipeline.environment.as_deref());
    let pipeline_task_names: Vec<String> = pipeline
        .tasks
        .iter()
        .map(|task| task.task_name().to_string())
        .collect();

    if task_selection == TaskSelection::All {
        validate_all_pipeline_tasks(pipeline, config)?;
    }

    // Bind the file references of everything the pipeline reaches, and
    // raise the problems recorded against those nodes, before any task is
    // selected: whichever tasks run, a reference that names no file or a
    // node that cannot run fails the pipeline that reaches it.
    let anchors = CheckoutAnchors {
        projects: project_map,
        module_root: module_prefix,
    };
    let project_root = frame.place(project_path).unwrap_or_default();
    let problems = crate::affected::reached_problems(
        &PipelineReach {
            project: config,
            project_root: &project_root,
            pipeline: &pipeline_name,
            pipeline_tasks: &pipeline_task_names,
            all_projects: project_map,
        },
        projects,
        &mut ReferenceChecks::new(frame.root()),
    );
    if !problems.is_empty() {
        return Err(cuenv_core::Error::configuration(format!(
            "Pipeline '{pipeline_name}' of project {} reaches what cannot run:\n  {}",
            project_path.display(),
            problems.join("\n  ")
        )));
    }

    let tasks_to_run = match (task_selection, changes) {
        // `--all` asks for every task. A run with no base gets every task
        // too: without a comparison none of them can be shown to be
        // unaffected, and there is nothing to place in the checkout's frame.
        // Inline and matrix entries run as they are written.
        (TaskSelection::All, _) | (TaskSelection::Affected, ChangedFiles::NoBase(_)) => {
            pipeline_task_names
        }
        (TaskSelection::Affected, ChangedFiles::Compared(changed_files)) => {
            if let Some(project_root) = frame.place(project_path) {
                crate::affected::compute_affected_tasks(
                    ChangedInputs {
                        files: changed_files,
                        project_root: &project_root,
                        anchors: &anchors,
                    },
                    &pipeline_task_names,
                    &AffectedProjects {
                        project: config,
                        all_projects: project_map,
                        checkout_root: Some(frame.root()),
                        changed_configuration: Some(changed_configuration),
                    },
                )
            } else {
                // Changed files are relative to the checkout. A project that
                // is not inside it cannot be shown to be unaffected by any
                // of them.
                cuenv_events::emit_stderr!(format!(
                    "warning: project {} is outside the checkout at {}, so its inputs cannot \
                     be compared with changes; every task of pipeline '{pipeline_name}' is \
                     selected",
                    project_path.display(),
                    frame.root().display()
                ));
                pipeline_task_names
            }
        }
    };

    if tasks_to_run.is_empty() {
        cuenv_events::emit_ci_project_skipped!(project_path.display(), "No affected tasks");
        return Ok(ProjectPlan::NothingAffected);
    }

    Ok(ProjectPlan::Run(ProjectPipelinePlan {
        pipeline_name,
        tasks_to_run,
        environment,
    }))
}

fn validate_all_pipeline_tasks(
    pipeline: &cuenv_core::ci::Pipeline,
    config: &Project,
) -> Result<()> {
    if pipeline.tasks.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "Requested --all pipeline has no tasks",
        ));
    }
    let index = TaskIndex::build(&config.tasks)?;
    for task in &pipeline.tasks {
        let PipelineTask::Simple(reference) = task else {
            return Err(cuenv_core::Error::configuration(
                "--all requires named task references; inline and matrix pipeline entries are unsupported",
            ));
        };
        index.resolve(reference.task_name())?;
    }
    Ok(())
}

fn format_pipeline_failures(failures: &[(String, cuenv_core::Error)]) -> String {
    failures
        .iter()
        .map(|(project, err)| format!("  [{project}]\n    {err}"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// All parameters needed to execute a project pipeline.
struct PipelineExecutionRequest<'request> {
    project_path: &'request Path,
    config: &'request Project,
    pipeline_name: &'request str,
    tasks_to_run: &'request [String],
    environment: Option<&'request str>,
    context: &'request crate::context::CIContext,
    changes: &'request ChangedFiles,
    provider: &'request dyn CIProvider,
    project_configs: &'request HashMap<PathBuf, Project>,
    all_projects: &'request Arc<CiProjects>,
    max_parallel: usize,
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
    let changes = request.changes;
    let changed_files = changes.files();
    let provider = request.provider;
    let project_configs = request.project_configs;
    let max_parallel = request.max_parallel;

    let start_time = Utc::now();
    let project_display = project_path.display().to_string();
    let cache_policy_override = super::reporting::cache_policy_override_for(context);

    // Register common CI secret patterns for redaction.
    // These are typically passed via GitHub Actions secrets or similar.
    super::reporting::register_ci_secrets();

    // Merge static + hook-generated environment once per project, then reuse for all tasks.
    let hook_env = request
        .all_projects
        .hook_environment(HookProject {
            root: project_path,
            config,
            ancestor_configs: project_configs,
        })
        .await?;

    // Build task index for resolving nested task names (e.g., "deploy.preview")
    let task_index = TaskIndex::build(&config.tasks)?;

    // The tasks about to run may depend on tasks of other projects, in or
    // out of `--path`. Each of those runs in its own project.
    let scope = Arc::new(super::task_execution::pipeline_scope(PipelineScope {
        project_path,
        config,
        task_index: &task_index,
        task_names: tasks_to_run,
        projects: request.all_projects,
    }));
    let project_configs = Arc::new(project_configs.clone());

    let PipelineTaskResults {
        reports: tasks_reports,
        status: pipeline_status,
        errors: task_errors,
        captures: all_captures,
    } = super::task_execution::execute_pipeline_tasks(PipelineTasksRequest {
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
        scope: &scope,
        projects: request.all_projects,
        project_configs: &project_configs,
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
        .map(|p| super::reporting::resolve_annotations(&p.annotations, &all_captures))
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
            no_base: changes.no_base().map(|why| why.reason().to_string()),
        },
        started_at: start_time,
        completed_at: Some(completed_at),
        duration_ms: Some(duration_ms),
        status: pipeline_status,
        tasks: tasks_reports,
        annotations: resolved_annotations,
    };

    // Write reports and notify provider
    super::reporting::write_pipeline_report(&report, context, project_path);
    super::reporting::notify_provider(provider, &report, pipeline_name).await;

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
    use crate::provider::NoBase;
    use chrono::Duration;

    fn selection_project() -> Project {
        serde_json::from_value(serde_json::json!({
            "name": "fixture",
            "ci": {"pipelines": {"verify": {"tasks": [
                {"_name": "unchanged"}, {"_name": "changed"}, {"_name": "always"}
            ]}}},
            "tasks": {
                "unchanged": {"command": "true", "inputs": ["other.txt"]},
                "changed": {"command": "true", "inputs": ["changed.txt"]},
                "always": {"command": "true"}
            }
        }))
        .unwrap()
    }

    /// A comparison in which only `changed.txt` differs from the base.
    fn one_changed_file() -> ChangedFiles {
        ChangedFiles::Compared(vec![PathBuf::from("changed.txt")])
    }

    /// What a provider reports for a run it cannot compare with anything.
    fn no_base() -> ChangedFiles {
        ChangedFiles::NoBase(NoBase::new("GitHub schedule event"))
    }

    fn plan_selection(
        config: &Project,
        selection: TaskSelection,
        changes: &ChangedFiles,
    ) -> Result<ProjectPlan> {
        plan_project_pipeline(ProjectPipelinePlanRequest {
            project_path: Path::new("/workspace"),
            config,
            scope: ProjectScope::Named,
            task_selection: selection,
            requested_pipeline: Some("verify"),
            requested_environment: None,
            changes,
            project_map: &HashMap::new(),
            frame: &CheckoutFrame::at(Path::new("/workspace")),
            module_prefix: Some(Path::new("")),
            projects: &CiProjects::new(CiModule {
                module_root: Path::new("/workspace"),
                checkout_root: Path::new("/workspace"),
                projects: &[],
                named: &HashMap::new(),
            }),
            changed_configuration: &HashSet::new(),
        })
    }

    fn planned_tasks(plan: ProjectPlan) -> Vec<String> {
        match plan {
            ProjectPlan::Run(plan) => plan.tasks_to_run,
            other => panic!("expected tasks to run, got {other:?}"),
        }
    }

    fn plan_in_scope(config: &Project, scope: ProjectScope, pipeline: &str) -> Result<ProjectPlan> {
        plan_project_pipeline(ProjectPipelinePlanRequest {
            project_path: Path::new("/workspace"),
            config,
            scope,
            task_selection: TaskSelection::Affected,
            requested_pipeline: Some(pipeline),
            requested_environment: None,
            changes: &one_changed_file(),
            project_map: &HashMap::new(),
            frame: &CheckoutFrame::at(Path::new("/workspace")),
            module_prefix: Some(Path::new("")),
            projects: &CiProjects::new(CiModule {
                module_root: Path::new("/workspace"),
                checkout_root: Path::new("/workspace"),
                projects: &[],
                named: &HashMap::new(),
            }),
            changed_configuration: &HashSet::new(),
        })
    }

    #[test]
    fn a_discovered_project_without_the_pipeline_is_skipped_and_a_named_one_is_an_error() {
        let config = selection_project();
        assert!(matches!(
            plan_in_scope(&config, ProjectScope::Discovered, "release").unwrap(),
            ProjectPlan::NoSuchPipeline
        ));
        assert!(
            plan_in_scope(&config, ProjectScope::Named, "release")
                .unwrap_err()
                .to_string()
                .contains("Pipeline 'release' not found")
        );

        let mut without_ci = selection_project();
        without_ci.ci = None;
        assert!(matches!(
            plan_in_scope(&without_ci, ProjectScope::Discovered, "verify").unwrap(),
            ProjectPlan::NoSuchPipeline
        ));
        assert!(
            plan_in_scope(&without_ci, ProjectScope::Named, "verify")
                .unwrap_err()
                .to_string()
                .contains("has no CI configuration")
        );
    }

    #[test]
    fn a_pipeline_no_project_defines_is_an_error_that_lists_those_that_exist() {
        let projects = vec![(PathBuf::from("/workspace"), selection_project())];
        let error = no_project_defines_pipeline("verfy", &projects).to_string();
        assert!(error.contains("pipeline 'verfy'"), "{error}");
        assert!(error.contains("Pipelines that exist: verify"), "{error}");

        let mut without_ci = selection_project();
        without_ci.ci = None;
        let error =
            no_project_defines_pipeline("verify", &[(PathBuf::from("/workspace"), without_ci)])
                .to_string();
        assert!(
            error.contains("none of them has a CI configuration"),
            "{error}"
        );
    }

    #[test]
    fn a_project_name_means_the_shallowest_directory_that_carries_it() {
        let mut content = selection_project();
        content.name = "content".into();
        let projects = vec![
            (PathBuf::from("/repo/content/drafts/deep"), content.clone()),
            (PathBuf::from("/repo/content"), content.clone()),
            (PathBuf::from("/repo/content/drafts"), content),
        ];
        for rotation in 0..projects.len() {
            let mut rotated = projects.clone();
            rotated.rotate_left(rotation);
            let map = build_project_map(&rotated);
            assert_eq!(map["content"].0, PathBuf::from("/repo/content"));
        }
    }

    #[test]
    fn all_selection_runs_unchanged_and_no_input_tasks() {
        let config = selection_project();
        let plan = planned_tasks(
            plan_selection(&config, TaskSelection::All, &one_changed_file()).unwrap(),
        );
        assert_eq!(plan, ["unchanged", "changed", "always"]);
    }

    #[test]
    fn default_selection_keeps_affected_filtering() {
        let config = selection_project();
        let plan = planned_tasks(
            plan_selection(&config, TaskSelection::Affected, &one_changed_file()).unwrap(),
        );
        assert_eq!(plan, ["changed", "always"]);
    }

    #[test]
    fn a_run_without_a_base_selects_every_task_of_the_pipeline() {
        let config = selection_project();
        let plan =
            planned_tasks(plan_selection(&config, TaskSelection::Affected, &no_base()).unwrap());
        assert_eq!(plan, ["unchanged", "changed", "always"]);
    }

    #[test]
    fn a_comparison_that_finds_nothing_is_not_a_missing_base() {
        let mut config = selection_project();
        config.tasks.remove("always");
        config
            .ci
            .as_mut()
            .unwrap()
            .pipelines
            .get_mut("verify")
            .unwrap()
            .tasks
            .pop();
        let nothing_changed = ChangedFiles::Compared(Vec::new());
        assert!(matches!(
            plan_selection(&config, TaskSelection::Affected, &nothing_changed).unwrap(),
            ProjectPlan::NothingAffected
        ));
    }

    #[test]
    fn a_run_without_a_base_keeps_the_inline_and_matrix_entries_all_rejects() {
        let mut config = selection_project();
        config
            .ci
            .as_mut()
            .unwrap()
            .pipelines
            .get_mut("verify")
            .unwrap()
            .tasks = serde_json::from_value(serde_json::json!([
            {"command": "true", "description": "inline"},
            {"type": "matrix", "task": {"_name": "unchanged"}, "matrix": {"arch": ["x64", "arm64"]}}
        ]))
        .unwrap();

        let plan =
            planned_tasks(plan_selection(&config, TaskSelection::Affected, &no_base()).unwrap());

        assert_eq!(plan, ["inline", "unchanged"]);
        assert!(
            plan_selection(&config, TaskSelection::All, &no_base())
                .unwrap_err()
                .to_string()
                .contains("named task references")
        );
    }

    #[test]
    fn all_selection_rejects_empty_and_unresolved_pipelines() {
        let mut config = selection_project();
        config
            .ci
            .as_mut()
            .unwrap()
            .pipelines
            .get_mut("verify")
            .unwrap()
            .tasks
            .clear();
        assert!(
            plan_selection(&config, TaskSelection::All, &one_changed_file())
                .unwrap_err()
                .to_string()
                .contains("has no tasks")
        );
        let mut config = selection_project();
        config.tasks.remove("unchanged");
        assert!(plan_selection(&config, TaskSelection::All, &one_changed_file()).is_err());
    }

    #[test]
    fn all_selection_rejects_inline_task_fallback_names() {
        let mut config = selection_project();
        config
            .ci
            .as_mut()
            .unwrap()
            .pipelines
            .get_mut("verify")
            .unwrap()
            .tasks = serde_json::from_value(
            serde_json::json!([{"command": "false", "description": "changed"}]),
        )
        .unwrap();
        assert!(
            plan_selection(&config, TaskSelection::All, &one_changed_file())
                .unwrap_err()
                .to_string()
                .contains("named task references")
        );
    }

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
