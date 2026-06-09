//! GitHub workflow sync operations.

mod matrix;

use super::{CiSyncOptions, ProjectInfo};
use cuenv_core::Result;
use cuenv_github::GitHubConfigExt;
use matrix::emit_matrix_workflow;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use tracing::instrument;

pub(super) struct GithubSyncRequest<'a> {
    pub(super) repo_root: &'a Path,
    pub(super) options: CiSyncOptions<'a>,
    pub(super) projects: &'a [ProjectInfo],
}

#[derive(Clone, Copy)]
struct GithubWorkflowFilesSyncRequest<'a> {
    workflows_dir: &'a Path,
    workflows: &'a [(String, String)],
    options: CiSyncOptions<'a>,
}

/// Sync GitHub Actions workflow files from CUE configuration.
#[instrument(name = "sync_github", skip_all)]
pub(super) async fn execute_sync_github(request: GithubSyncRequest<'_>) -> Result<String> {
    let GithubSyncRequest {
        repo_root,
        options,
        projects,
    } = request;
    if projects.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "No cuenv projects found. Ensure env.cue files declare 'package cuenv'",
        ));
    }

    // Generate workflows per-project, per-pipeline
    // Each project with CI config gets its own workflow files
    let mut all_workflows: Vec<(String, String)> = Vec::new();
    for project in projects {
        let Some(ci) = &project.config.ci else {
            continue;
        };
        for (pipeline_name, pipeline) in &ci.pipelines {
            let workflows = generate_github_workflow_for_project(project, pipeline_name, pipeline)?;
            all_workflows.extend(workflows);
        }
    }

    if all_workflows.is_empty() {
        return Ok(String::new());
    }

    let workflows_dir = repo_root.join(".github/workflows");
    sync_github_workflow_files(GithubWorkflowFilesSyncRequest {
        workflows_dir: &workflows_dir,
        workflows: &all_workflows,
        options,
    })
}

fn sync_github_workflow_files(request: GithubWorkflowFilesSyncRequest<'_>) -> Result<String> {
    let GithubWorkflowFilesSyncRequest {
        workflows_dir,
        workflows,
        options,
    } = request;
    let mut output_lines = Vec::new();

    // Check mode: compare generated content with existing files
    if options.check {
        let mut out_of_sync = Vec::new();
        for (filename, content) in workflows {
            let path = workflows_dir.join(filename);
            if path.exists() {
                let existing =
                    std::fs::read_to_string(&path).map_err(|e| cuenv_core::Error::Io {
                        source: e,
                        path: Some(path.clone().into_boxed_path()),
                        operation: "read workflow file".to_string(),
                    })?;
                if existing != *content {
                    out_of_sync.push(filename.clone());
                }
            } else {
                out_of_sync.push(format!("{filename} (missing)"));
            }
        }
        if !out_of_sync.is_empty() {
            return Err(cuenv_core::Error::configuration(format!(
                "GitHub workflows out of sync: {}. Run 'cuenv sync ci' to update.",
                out_of_sync.join(", ")
            )));
        }
        return Ok(format!("GitHub: {} workflow(s) in sync", workflows.len()));
    }

    // Dry-run or normal mode
    for (filename, content) in workflows {
        let workflow_path = workflows_dir.join(filename);
        let exists = workflow_path.exists();

        // Check if content matches (skip if unchanged)
        if exists && !options.dry_run.is_dry_run() {
            let existing = std::fs::read_to_string(&workflow_path).unwrap_or_default();
            if existing == *content {
                output_lines.push(format!("GitHub: {filename} (unchanged)"));
                continue;
            }
        }

        if options.dry_run.is_dry_run() {
            if exists {
                output_lines.push(format!("GitHub: Would update {filename}"));
            } else {
                output_lines.push(format!("GitHub: Would create {filename}"));
            }
        } else {
            // Create directory if needed
            if !workflows_dir.exists() {
                std::fs::create_dir_all(workflows_dir).map_err(|e| cuenv_core::Error::Io {
                    source: e,
                    path: Some(workflows_dir.to_path_buf().into_boxed_path()),
                    operation: "create directory".to_string(),
                })?;
            }

            std::fs::write(&workflow_path, content).map_err(|e| cuenv_core::Error::Io {
                source: e,
                path: Some(workflow_path.clone().into_boxed_path()),
                operation: "write workflow file".to_string(),
            })?;

            if exists {
                output_lines.push(format!("GitHub: Updated {filename}"));
            } else {
                output_lines.push(format!("GitHub: Created {filename}"));
            }
        }
    }

    Ok(output_lines.join("\n"))
}

/// Collected pipeline context from project discovery.
struct PipelineContext {
    is_release: bool,
    cuenv_source: cuenv_core::config::CuenvSource,
    /// Pipeline generation mode (thin vs expanded)
    mode: cuenv_core::ci::PipelineMode,
    github_config: cuenv_github::config::GitHubConfig,
    trigger: cuenv_ci::ir::TriggerCondition,
    project_name: Option<String>,
    /// Relative path to project directory (for working-directory in monorepos)
    project_path: Option<String>,
    environment: Option<String>,
    runtimes: Vec<cuenv_ci::ir::Runtime>,
    /// All tasks including phase tasks (phase tasks have phase field set)
    tasks: Vec<cuenv_ci::ir::Task>,
    /// Original pipeline tasks (with matrix/artifacts/params info)
    pipeline_tasks: Vec<cuenv_core::ci::PipelineTask>,
}

impl PipelineContext {
    /// Build an IntermediateRepresentation from this context.
    fn to_ir(&self, pipeline_name: &str) -> cuenv_ci::ir::IntermediateRepresentation {
        cuenv_ci::ir::IntermediateRepresentation {
            version: "1.5".to_string(),
            pipeline: cuenv_ci::ir::PipelineMetadata {
                name: pipeline_name.to_string(),
                mode: self.mode,
                environment: self.environment.clone(),
                requires_onepassword: false,
                project_name: self.project_name.clone(),
                project_path: self.project_path.clone(),
                trigger: Some(self.trigger.clone()),
                pipeline_tasks: self
                    .pipeline_tasks
                    .iter()
                    .map(|t| t.task_name().to_string())
                    .collect(),
                pipeline_task_defs: self.pipeline_tasks.clone(),
            },
            runtimes: self.runtimes.clone(),
            tasks: self.tasks.clone(),
        }
    }

    /// Get regular (non-phase) tasks from this context.
    fn regular_tasks(&self) -> Vec<&cuenv_ci::ir::Task> {
        self.tasks.iter().filter(|t| t.phase.is_none()).collect()
    }
}

/// Check if any pipeline tasks have matrix configurations that require expansion.
///
/// Returns true only for tasks with actual matrix dimensions (non-empty matrix map).
/// Aggregation tasks (empty matrix with artifacts) return false.
pub(super) fn has_matrix_tasks(pipeline_tasks: &[cuenv_core::ci::PipelineTask]) -> bool {
    pipeline_tasks
        .iter()
        .any(cuenv_core::ci::PipelineTask::has_matrix_dimensions)
}

/// Generate GitHub workflow files for a single project and pipeline.
fn generate_github_workflow_for_project(
    project: &ProjectInfo,
    pipeline_name: &str,
    pipeline: &cuenv_core::ci::Pipeline,
) -> Result<Vec<(String, String)>> {
    use cuenv_core::ci::PipelineMode;

    let ctx = build_project_pipeline_context(project, pipeline_name, pipeline)?;

    // Dispatch based on pipeline mode
    // Note: Matrix tasks ALWAYS require multi-job workflow regardless of mode,
    // since they need to run on different runners for each matrix dimension.
    match ctx.mode {
        PipelineMode::Thin => {
            // Thin mode with matrix tasks still needs multi-job workflow
            if has_matrix_tasks(&ctx.pipeline_tasks) {
                emit_matrix_workflow(pipeline_name, &ctx)
            } else {
                // Pure thin mode: single job with cuenv ci orchestration
                emit_thin_workflow(pipeline_name, &ctx)
            }
        }
        PipelineMode::Expanded => {
            // Expanded mode: all tasks as individual jobs with dependencies
            if has_matrix_tasks(&ctx.pipeline_tasks) {
                emit_matrix_workflow(pipeline_name, &ctx)
            } else if ctx.is_release {
                emit_release_workflow(pipeline_name, &ctx)
            } else if ctx.tasks.is_empty() {
                Ok(Vec::new())
            } else {
                emit_standard_workflow(pipeline_name, &ctx)
            }
        }
    }
}

/// Build pipeline context for a single project and pipeline.
fn build_project_pipeline_context(
    project: &ProjectInfo,
    pipeline_name: &str,
    pipeline: &cuenv_core::ci::Pipeline,
) -> Result<PipelineContext> {
    use cuenv_ci::compiler::{Compiler, CompilerOptions};

    let ci = project
        .config
        .ci
        .as_ref()
        .ok_or_else(|| cuenv_core::Error::configuration("Project has no CI configuration"))?;
    let cuenv_source = project
        .config
        .config
        .as_ref()
        .and_then(|config| config.ci.as_ref())
        .and_then(|ci| ci.cuenv.as_ref())
        .map_or(cuenv_core::config::CuenvSource::Release, |cuenv| {
            cuenv.source
        });

    // Detect release pipelines by checking if they have release event triggers
    let is_release = pipeline.when.as_ref().is_some_and(|w| w.release.is_some());

    // Compute project_path for compiler (None if root, i.e., empty relative_path)
    let project_path_for_compiler = if project.relative_path.as_os_str().is_empty() {
        None
    } else {
        Some(project.relative_path.to_string_lossy().to_string())
    };

    let options = CompilerOptions {
        pipeline_name: Some(pipeline_name.to_string()),
        pipeline: Some(pipeline.clone()),
        ci_mode: true,
        module_root: Some(project.module_root.clone()),
        project_path: project_path_for_compiler.clone(),
        ..Default::default()
    };
    let compiler = Compiler::with_options(project.config.clone(), options);
    let ir = compiler
        .compile()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to compile project: {e}")))?;

    // Extract task names from pipeline tasks (which can be simple strings or matrix tasks)
    let pipeline_task_names: Vec<String> = pipeline
        .tasks
        .iter()
        .map(|t| t.task_name().to_string())
        .collect();

    // Get pipeline tasks (non-phase tasks)
    let filtered_tasks = cuenv_ci::pipeline::filter_tasks(&pipeline_task_names, ir.tasks.clone());

    // Combine phase tasks (bootstrap, setup, success, failure) with pipeline tasks
    let phase_tasks: Vec<cuenv_ci::ir::Task> =
        ir.tasks.into_iter().filter(|t| t.phase.is_some()).collect();
    let mut all_tasks = phase_tasks;
    all_tasks.extend(filtered_tasks);

    // Use the compiler-derived trigger which includes paths from task inputs
    let trigger = ir
        .pipeline
        .trigger
        .unwrap_or_else(|| build_github_trigger_condition(pipeline_name, pipeline, ci));

    Ok(PipelineContext {
        is_release,
        cuenv_source,
        mode: pipeline.mode,
        github_config: ci.github_config_for_pipeline(pipeline_name),
        trigger,
        project_name: Some(project.config.name.clone()),
        project_path: project_path_for_compiler,
        environment: pipeline.environment.clone(),
        runtimes: ir.runtimes,
        tasks: all_tasks,
        pipeline_tasks: pipeline.tasks.clone(),
    })
}

/// Emit a release workflow using the `ReleaseWorkflowBuilder`.
fn emit_release_workflow(
    pipeline_name: &str,
    ctx: &PipelineContext,
) -> Result<Vec<(String, String)>> {
    use cuenv_github::workflow::{GitHubActionsEmitter, ReleaseWorkflowBuilder};

    let ir = ctx.to_ir(pipeline_name);

    let emitter = GitHubActionsEmitter::from_config(&ctx.github_config).with_nix();
    let workflow = ReleaseWorkflowBuilder::new(emitter).build(&ir);

    let workflow_name = match &ir.pipeline.project_name {
        Some(project) => format!("{project}-{}", ir.pipeline.name),
        None => ir.pipeline.name.clone(),
    };
    let filename = format!("{}.yml", sanitize_workflow_name(&workflow_name));

    let yaml = workflow.to_yaml().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to serialize workflow: {e}"))
    })?;

    Ok(vec![(filename, yaml)])
}

/// Emit a thin mode workflow by delegating to the GitHub Actions emitter.
fn emit_thin_workflow(pipeline_name: &str, ctx: &PipelineContext) -> Result<Vec<(String, String)>> {
    use cuenv_github::workflow::GitHubActionsEmitter;

    let ir = ctx.to_ir(pipeline_name);
    let emitter = GitHubActionsEmitter::from_config(&ctx.github_config).with_nix();

    let (filename, yaml) = emitter.emit_thin_workflow(&ir).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to emit thin workflow: {e}"))
    })?;

    Ok(vec![(filename, yaml)])
}

fn runner_key(runs_on: &cuenv_github::workflow::schema::RunsOn) -> String {
    match runs_on {
        cuenv_github::workflow::schema::RunsOn::Label(label) => format!("label:{label}"),
        cuenv_github::workflow::schema::RunsOn::Labels(labels) => {
            format!("labels:{}", labels.join("|"))
        }
    }
}

fn runner_suffix(runs_on: &cuenv_github::workflow::schema::RunsOn) -> String {
    let raw = match runs_on {
        cuenv_github::workflow::schema::RunsOn::Label(label) => label.clone(),
        cuenv_github::workflow::schema::RunsOn::Labels(labels) => labels.join("-"),
    };

    raw.to_lowercase()
        .replace(['.', ' '], "-")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

fn prepend_need(job: &mut cuenv_github::workflow::schema::Job, dependency: &str) {
    if job.needs.iter().any(|need| need == dependency) {
        return;
    }

    let mut needs = Vec::with_capacity(job.needs.len() + 1);
    needs.push(dependency.to_string());
    needs.extend(job.needs.clone());
    job.needs = needs;
}

fn cuenv_bootstrap_artifact_name(runs_on: &cuenv_github::workflow::schema::RunsOn) -> String {
    format!("cuenv-bootstrap-{}", runner_suffix(runs_on))
}

fn can_use_cuenv_bootstrap(
    ctx: &PipelineContext,
    emitter: &cuenv_github::workflow::GitHubActionsEmitter,
    ir: &cuenv_ci::ir::IntermediateRepresentation,
) -> bool {
    ctx.cuenv_source == cuenv_core::config::CuenvSource::Nix
        && emitter.build_cuenv
        && ir
            .sorted_phase_tasks(cuenv_ci::ir::BuildStage::Setup)
            .iter()
            .any(|task| task.contributor.as_deref() == Some("cuenv"))
}

fn inject_cuenv_bootstrap_jobs(
    jobs: &mut indexmap::IndexMap<String, cuenv_github::workflow::schema::Job>,
    ir: &cuenv_ci::ir::IntermediateRepresentation,
    emitter: &cuenv_github::workflow::GitHubActionsEmitter,
    bootstrap_consumers: &HashSet<String>,
) {
    use cuenv_github::workflow::schema::RunsOn;
    use indexmap::IndexMap;

    if jobs.is_empty() || bootstrap_consumers.is_empty() {
        return;
    }

    let mut runners = IndexMap::<String, RunsOn>::new();
    for (job_id, job) in jobs.iter() {
        if !bootstrap_consumers.contains(job_id) {
            continue;
        }

        runners
            .entry(runner_key(&job.runs_on))
            .or_insert_with(|| job.runs_on.clone());
    }

    if runners.is_empty() {
        return;
    }

    let multiple_runners = runners.len() > 1;
    let mut runner_bootstrap_jobs = IndexMap::<String, String>::new();
    let mut bootstrap_jobs = IndexMap::<String, cuenv_github::workflow::schema::Job>::new();

    for (key, runs_on) in runners {
        let (bootstrap_job_id, display_name) = if multiple_runners {
            let suffix = runner_suffix(&runs_on);
            (
                format!("build-cuenv-{suffix}"),
                format!("build.cuenv ({suffix})"),
            )
        } else {
            ("build-cuenv".to_string(), "build.cuenv".to_string())
        };

        let artifact_name = cuenv_bootstrap_artifact_name(&runs_on);
        let Some(job) = emitter.build_cuenv_bootstrap_job(
            ir,
            cuenv_github::workflow::CuenvBootstrapJobOptions {
                runs_on,
                name: &display_name,
                artifact_name: &artifact_name,
            },
        ) else {
            return;
        };

        runner_bootstrap_jobs.insert(key, bootstrap_job_id.clone());
        bootstrap_jobs.insert(bootstrap_job_id, job);
    }

    for (job_id, job) in jobs.iter_mut() {
        if !bootstrap_consumers.contains(job_id) {
            continue;
        }

        if let Some(bootstrap_job_id) = runner_bootstrap_jobs.get(&runner_key(&job.runs_on)) {
            prepend_need(job, bootstrap_job_id);
        }
    }

    let existing_jobs = std::mem::take(jobs);
    jobs.extend(bootstrap_jobs);
    jobs.extend(existing_jobs);
}

/// Emit a standard workflow using the `GitHubActionsEmitter`.
///
/// Builds jobs directly using `build_simple_job` which supports `project_path`
/// for setting working-directory in monorepo workflows.
fn simple_job_execution(
    ctx: &PipelineContext,
    task: &cuenv_ci::ir::Task,
) -> cuenv_github::workflow::TaskExecution {
    let is_direct_nix_job =
        !ctx.is_release && task.command.first().is_some_and(|command| command == "nix");

    if is_direct_nix_job {
        cuenv_github::workflow::TaskExecution::Direct
    } else {
        cuenv_github::workflow::TaskExecution::Orchestrated
    }
}

fn simple_job_options<'a>(
    ctx: &'a PipelineContext,
    task: &cuenv_ci::ir::Task,
    cuenv_artifact_name: Option<String>,
) -> cuenv_github::workflow::SimpleJobOptions<'a> {
    if simple_job_execution(ctx, task) == cuenv_github::workflow::TaskExecution::Direct {
        cuenv_github::workflow::SimpleJobOptions::direct(ctx.project_path.as_deref())
    } else if let Some(artifact_name) = cuenv_artifact_name {
        cuenv_github::workflow::SimpleJobOptions::orchestrated_with_cuenv_artifact(
            ctx.environment.as_ref(),
            ctx.project_path.as_deref(),
            artifact_name,
        )
    } else {
        cuenv_github::workflow::SimpleJobOptions::orchestrated(
            ctx.environment.as_ref(),
            ctx.project_path.as_deref(),
        )
    }
}

fn emit_standard_workflow(
    pipeline_name: &str,
    ctx: &PipelineContext,
) -> Result<Vec<(String, String)>> {
    use cuenv_github::workflow::GitHubActionsEmitter;
    use cuenv_github::workflow::schema::{Concurrency, Workflow};
    use indexmap::IndexMap;

    let workflow_name = match &ctx.project_name {
        Some(project) => format!("{project}-{pipeline_name}"),
        None => pipeline_name.to_string(),
    };

    let ir = ctx.to_ir(pipeline_name);
    let emitter = GitHubActionsEmitter::from_config(&ctx.github_config).with_nix();
    let use_cuenv_bootstrap = can_use_cuenv_bootstrap(ctx, &emitter, &ir);
    let default_runner = cuenv_github::workflow::schema::RunsOn::Label(emitter.runner.clone());
    let default_artifact_name =
        use_cuenv_bootstrap.then(|| cuenv_bootstrap_artifact_name(&default_runner));

    // Build jobs using build_simple_job (which supports project_path for working-directory)
    // Only iterate over regular tasks (non-phase tasks) - phase tasks are handled internally
    let mut jobs = IndexMap::new();
    let mut bootstrap_consumers = HashSet::new();
    for task in ctx.regular_tasks() {
        let job_id = task.id.replace(['.', ' '], "-");
        let execution = simple_job_execution(ctx, task);
        let artifact_name = if execution == cuenv_github::workflow::TaskExecution::Orchestrated {
            default_artifact_name.clone()
        } else {
            None
        };
        let mut job =
            emitter.build_simple_job(task, &ir, &simple_job_options(ctx, task, artifact_name));
        job.needs = task
            .depends_on
            .iter()
            .map(|d| d.replace(['.', ' '], "-"))
            .collect();
        if use_cuenv_bootstrap && execution == cuenv_github::workflow::TaskExecution::Orchestrated {
            bootstrap_consumers.insert(job_id.clone());
        }
        jobs.insert(job_id, job);
    }

    inject_cuenv_bootstrap_jobs(&mut jobs, &ir, &emitter, &bootstrap_consumers);

    let filename = format!("{}.yml", sanitize_workflow_name(&workflow_name));

    let workflow = Workflow {
        name: workflow_name.clone(),
        on: emitter.build_triggers(&ir, &filename),
        concurrency: Some(Concurrency {
            group: "${{ github.workflow }}-${{ github.head_ref || github.ref }}".to_string(),
            cancel_in_progress: Some(true),
        }),
        permissions: Some(emitter.build_permissions(&ir)),
        env: IndexMap::new(),
        jobs,
    };
    let yaml = workflow.to_yaml().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to serialize workflow: {e}"))
    })?;

    Ok(vec![(filename, yaml)])
}

/// Sanitize a workflow name for use as a filename.
fn sanitize_workflow_name(name: &str) -> String {
    name.to_lowercase()
        .replace(' ', "-")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// Build GitHub Actions trigger condition from pipeline config.
fn build_github_trigger_condition(
    _pipeline_name: &str,
    pipeline: &cuenv_core::ci::Pipeline,
    _ci_config: &cuenv_core::ci::CI,
) -> cuenv_ci::ir::TriggerCondition {
    use cuenv_ci::ir::{ManualTriggerConfig, TriggerCondition, WorkflowDispatchInputDef};
    use cuenv_core::ci::ManualTrigger;

    let when = pipeline.when.as_ref();

    let branches = when
        .and_then(|w| w.branch.as_ref())
        .map(cuenv_core::ci::StringOrVec::to_vec)
        .unwrap_or_default();

    let pull_request = when.and_then(|w| w.pull_request);

    let scheduled = when
        .and_then(|w| w.scheduled.as_ref())
        .map(cuenv_core::ci::StringOrVec::to_vec)
        .unwrap_or_default();

    let release = when.and_then(|w| w.release.clone()).unwrap_or_default();

    let manual = when.and_then(|w| w.manual.as_ref()).map(|m| match m {
        ManualTrigger::Enabled(enabled) => ManualTriggerConfig {
            enabled: *enabled,
            inputs: BTreeMap::new(),
        },
        ManualTrigger::WithInputs(inputs) => ManualTriggerConfig {
            enabled: true,
            inputs: inputs
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        WorkflowDispatchInputDef {
                            description: v.description.clone(),
                            required: v.required.unwrap_or(false),
                            default: v.default.clone(),
                            input_type: v.input_type.clone(),
                            options: v.options.clone().unwrap_or_default(),
                        },
                    )
                })
                .collect(),
        },
    });

    TriggerCondition {
        branches,
        pull_request,
        scheduled,
        release,
        manual,
        paths: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_ci::ir::{
        BuildStage, CachePolicy, IntermediateRepresentation, PipelineMetadata, Task,
    };
    use cuenv_core::ci::PipelineMode;
    use cuenv_github::workflow::schema::{Job, RunsOn};
    use indexmap::IndexMap;
    use std::collections::{BTreeMap, HashSet};

    struct PhaseTaskFixture<'a> {
        id: &'a str,
        contributor: &'a str,
        phase: BuildStage,
        priority: i32,
        label: &'a str,
        command: &'a [&'a str],
    }

    fn make_ir(tasks: Vec<Task>) -> IntermediateRepresentation {
        IntermediateRepresentation {
            version: "1.5".to_string(),
            pipeline: PipelineMetadata {
                name: "ci".to_string(),
                mode: PipelineMode::Expanded,
                environment: None,
                requires_onepassword: false,
                project_name: None,
                project_path: None,
                trigger: None,
                pipeline_tasks: vec![],
                pipeline_task_defs: vec![],
            },
            runtimes: vec![],
            tasks,
        }
    }

    fn make_phase_task(fixture: &PhaseTaskFixture<'_>) -> Task {
        Task {
            id: fixture.id.to_string(),
            runtime: None,
            command: fixture
                .command
                .iter()
                .map(|part| (*part).to_string())
                .collect(),
            shell: fixture.command.len() == 1,
            env: BTreeMap::new(),
            secrets: BTreeMap::new(),
            resources: None,
            concurrency_group: None,
            inputs: vec![],
            outputs: vec![],
            depends_on: vec![],
            cache_policy: CachePolicy::Disabled,
            deployment: false,
            manual_approval: false,
            matrix: None,
            artifact_downloads: vec![],
            params: BTreeMap::new(),
            phase: Some(fixture.phase),
            label: Some(fixture.label.to_string()),
            priority: Some(fixture.priority),
            contributor: Some(fixture.contributor.to_string()),
            condition: None,
            provider_hints: None,
        }
    }

    fn make_job(runs_on: RunsOn) -> Job {
        Job {
            name: Some("job".to_string()),
            runs_on,
            needs: Vec::new(),
            if_condition: None,
            strategy: None,
            environment: None,
            env: IndexMap::new(),
            concurrency: None,
            continue_on_error: None,
            timeout_minutes: None,
            steps: Vec::new(),
        }
    }

    fn make_regular_task(id: &str) -> Task {
        Task {
            id: id.to_string(),
            runtime: None,
            command: vec!["cargo".to_string(), "test".to_string()],
            shell: false,
            env: BTreeMap::new(),
            secrets: BTreeMap::new(),
            resources: None,
            concurrency_group: None,
            inputs: vec![],
            outputs: vec![],
            depends_on: vec![],
            cache_policy: CachePolicy::Disabled,
            deployment: false,
            manual_approval: false,
            matrix: None,
            artifact_downloads: vec![],
            params: BTreeMap::new(),
            phase: None,
            label: None,
            priority: None,
            contributor: None,
            condition: None,
            provider_hints: None,
        }
    }

    fn make_bootstrap_ir() -> IntermediateRepresentation {
        make_ir(vec![
            make_phase_task(&PhaseTaskFixture {
                id: "cuenv:contributor:nix.install",
                contributor: "nix",
                phase: BuildStage::Bootstrap,
                priority: 0,
                label: "Install Nix",
                command: &["install nix"],
            }),
            make_phase_task(&PhaseTaskFixture {
                id: "cuenv:contributor:cuenv.setup",
                contributor: "cuenv",
                phase: BuildStage::Setup,
                priority: 10,
                label: "Build cuenv (nix)",
                command: &["nix build .#cuenv"],
            }),
        ])
    }

    fn make_pipeline_context(
        cuenv_source: cuenv_core::config::CuenvSource,
        tasks: Vec<Task>,
    ) -> PipelineContext {
        PipelineContext {
            is_release: false,
            cuenv_source,
            mode: PipelineMode::Expanded,
            github_config: cuenv_github::config::GitHubConfig::default(),
            trigger: cuenv_ci::ir::TriggerCondition {
                branches: vec![],
                pull_request: None,
                scheduled: vec![],
                release: vec![],
                manual: None,
                paths: vec![],
            },
            project_name: Some("test".to_string()),
            project_path: None,
            environment: None,
            runtimes: vec![],
            tasks,
            pipeline_tasks: vec![cuenv_core::ci::PipelineTask::Simple(
                cuenv_core::ci::TaskRef::from_name("build"),
            )],
        }
    }

    #[test]
    fn nix_source_standard_workflow_emits_cuenv_bootstrap_artifact() -> cuenv_core::Result<()> {
        let mut tasks = make_bootstrap_ir().tasks;
        tasks.push(make_regular_task("build"));
        let ctx = make_pipeline_context(cuenv_core::config::CuenvSource::Nix, tasks);

        let workflows = emit_standard_workflow("ci", &ctx)?;

        assert_eq!(workflows.len(), 1);
        let workflow = &workflows[0].1;
        assert!(workflow.contains("build.cuenv"), "{workflow}");
        assert!(workflow.contains("result/bin/cuenv"), "{workflow}");
        assert!(
            workflow.contains("actions/upload-artifact@v4"),
            "{workflow}"
        );
        Ok(())
    }

    #[test]
    fn bootstrap_injection_skips_all_direct_jobs() {
        let ir = make_bootstrap_ir();
        let emitter = cuenv_github::workflow::GitHubActionsEmitter::new().with_nix();
        let mut jobs = IndexMap::new();
        jobs.insert(
            "checks-nextest".to_string(),
            make_job(RunsOn::Label("ubuntu-latest".to_string())),
        );

        inject_cuenv_bootstrap_jobs(&mut jobs, &ir, &emitter, &HashSet::new());

        assert!(!jobs.contains_key("build-cuenv"));
        assert!(jobs["checks-nextest"].needs.is_empty());
    }

    #[test]
    fn bootstrap_injection_targets_only_orchestrated_runner_consumers() {
        let ir = make_bootstrap_ir();
        let emitter = cuenv_github::workflow::GitHubActionsEmitter::new().with_nix();
        let mut jobs = IndexMap::new();
        jobs.insert(
            "checks-nextest".to_string(),
            make_job(RunsOn::Label("macos-14".to_string())),
        );
        jobs.insert(
            "publish-github".to_string(),
            make_job(RunsOn::Label("ubuntu-latest".to_string())),
        );
        let bootstrap_consumers = HashSet::from(["publish-github".to_string()]);

        inject_cuenv_bootstrap_jobs(&mut jobs, &ir, &emitter, &bootstrap_consumers);

        assert_eq!(jobs.len(), 3);
        assert!(jobs.contains_key("build-cuenv"));
        assert!(jobs["checks-nextest"].needs.is_empty());
        assert_eq!(jobs["publish-github"].needs, vec!["build-cuenv"]);

        let bootstrap = &jobs["build-cuenv"];
        assert!(matches!(
            bootstrap.runs_on,
            RunsOn::Label(ref label) if label == "ubuntu-latest"
        ));
        assert!(
            bootstrap
                .steps
                .iter()
                .any(|step| step.name.as_deref() == Some("Upload cuenv"))
        );
    }
}
