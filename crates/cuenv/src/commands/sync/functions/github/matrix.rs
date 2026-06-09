//! GitHub Actions matrix workflow emission.

use super::{
    PipelineContext, can_use_cuenv_bootstrap, cuenv_bootstrap_artifact_name,
    inject_cuenv_bootstrap_jobs, sanitize_workflow_name, simple_job_execution, simple_job_options,
};
use cuenv_core::Result;
use std::collections::{BTreeMap, HashMap, HashSet};

struct BuiltPipelineJobs {
    jobs: indexmap::IndexMap<String, cuenv_github::workflow::schema::Job>,
    bootstrap_consumers: HashSet<String>,
}

struct PipelineJobBuildContext<'a> {
    ctx: &'a PipelineContext,
    ir: &'a cuenv_ci::ir::IntermediateRepresentation,
    emitter: &'a cuenv_github::workflow::GitHubActionsEmitter,
    use_cuenv_bootstrap: bool,
    default_cuenv_setup: cuenv_github::workflow::CuenvSetup,
}

struct PipelineJobsRequest<'a> {
    expanded_tasks: &'a [cuenv_core::ci::PipelineTask],
    ctx: &'a PipelineContext,
    ir: &'a cuenv_ci::ir::IntermediateRepresentation,
    emitter: &'a cuenv_github::workflow::GitHubActionsEmitter,
    use_cuenv_bootstrap: bool,
}

struct TransitiveDependencyJobRequest<'a> {
    jobs: &'a mut indexmap::IndexMap<String, cuenv_github::workflow::schema::Job>,
    bootstrap_consumers: &'a mut HashSet<String>,
    processed_task_names: &'a HashSet<String>,
    build_context: &'a PipelineJobBuildContext<'a>,
}

/// Emit a workflow with matrix expansion for tasks that have matrix configurations.
pub(super) fn emit_matrix_workflow(
    pipeline_name: &str,
    ctx: &PipelineContext,
) -> Result<Vec<(String, String)>> {
    use cuenv_github::workflow::GitHubActionsEmitter;
    use cuenv_github::workflow::schema::{Concurrency, Workflow};

    let workflow_name = match &ctx.project_name {
        Some(project) => format!("{project}-{pipeline_name}"),
        None => pipeline_name.to_string(),
    };

    let ir = ctx.to_ir(pipeline_name);
    let emitter = GitHubActionsEmitter::from_config(&ctx.github_config).with_nix();
    let use_cuenv_bootstrap = can_use_cuenv_bootstrap(ctx, &emitter, &ir);

    let explicit_task_names: HashSet<String> = ctx
        .pipeline_tasks
        .iter()
        .map(|pt| pt.task_name().to_string())
        .collect();

    let expanded_tasks = cuenv_ci::pipeline::expand_task_groups(
        &ctx.pipeline_tasks,
        &ctx.tasks,
        &explicit_task_names,
    );

    let built_jobs = build_pipeline_jobs(&PipelineJobsRequest {
        expanded_tasks: &expanded_tasks,
        ctx,
        ir: &ir,
        emitter: &emitter,
        use_cuenv_bootstrap,
    });
    let mut jobs = built_jobs.jobs;
    inject_cuenv_bootstrap_jobs(&mut jobs, &ir, &emitter, &built_jobs.bootstrap_consumers);

    let filename = format!("{}.yml", sanitize_workflow_name(&workflow_name));

    let workflow = Workflow {
        name: workflow_name.clone(),
        on: emitter.build_triggers(&ir, &filename),
        concurrency: Some(Concurrency {
            group: "${{ github.workflow }}-${{ github.head_ref || github.ref }}".to_string(),
            cancel_in_progress: Some(true),
        }),
        permissions: Some(emitter.build_permissions(&ir)),
        env: indexmap::IndexMap::new(),
        jobs,
    };
    let yaml = workflow.to_yaml().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to serialize workflow: {e}"))
    })?;

    Ok(vec![(filename, yaml)])
}

/// Build jobs from expanded pipeline tasks, tracking artifact sources.
///
/// Uses `GitHubActionsEmitter` methods to build jobs, converting `PipelineTask`
/// info to IR `Task` fields as needed.
fn build_pipeline_jobs(request: &PipelineJobsRequest<'_>) -> BuiltPipelineJobs {
    use indexmap::IndexMap;

    let mut jobs = IndexMap::new();
    let mut bootstrap_consumers = HashSet::new();
    let mut artifact_source_jobs: HashSet<String> = HashSet::new();
    let mut processed_task_names: HashSet<String> = HashSet::new();
    let cuenv_artifacts_by_runner = if request.use_cuenv_bootstrap {
        Some(cuenv_artifacts_by_runner(request.ctx, request.emitter))
    } else {
        None
    };
    let default_cuenv_setup = cuenv_setup_for_runner(
        request.emitter.runner.as_str(),
        cuenv_artifacts_by_runner.as_ref(),
    );
    let build_context = PipelineJobBuildContext {
        ctx: request.ctx,
        ir: request.ir,
        emitter: request.emitter,
        use_cuenv_bootstrap: request.use_cuenv_bootstrap,
        default_cuenv_setup: default_cuenv_setup.clone(),
    };

    for pipeline_task in request.expanded_tasks {
        let task_name = pipeline_task.task_name();
        processed_task_names.insert(task_name.to_string());
        let job_id = task_name.replace(['.', ' '], "-");

        match pipeline_task {
            cuenv_core::ci::PipelineTask::Simple(_) | cuenv_core::ci::PipelineTask::Node(_) => {
                if let Some(ir_task) = request.ctx.tasks.iter().find(|t| t.id == task_name) {
                    let execution = simple_job_execution(request.ctx, ir_task);
                    let artifact_name =
                        if execution == cuenv_github::workflow::TaskExecution::Orchestrated {
                            default_cuenv_setup.artifact_name()
                        } else {
                            None
                        };
                    let mut job = request.emitter.build_simple_job(
                        ir_task,
                        request.ir,
                        &simple_job_options(request.ctx, ir_task, artifact_name),
                    );
                    job.needs = ir_task
                        .depends_on
                        .iter()
                        .map(|dep| dep.replace(['.', ' '], "-"))
                        .collect();
                    if request.use_cuenv_bootstrap
                        && execution == cuenv_github::workflow::TaskExecution::Orchestrated
                    {
                        bootstrap_consumers.insert(job_id.clone());
                    }
                    jobs.insert(job_id, job);
                }
            }
            cuenv_core::ci::PipelineTask::Matrix(matrix_task) => {
                if matrix_task.matrix.is_empty() {
                    let ir_task = request.ctx.tasks.iter().find(|t| t.id == task_name);
                    let mut seen: HashSet<String> = artifact_source_jobs.clone();
                    let mut combined_needs: Vec<String> =
                        artifact_source_jobs.iter().cloned().collect();

                    if let Some(ir_task) = ir_task {
                        for dep in &ir_task.depends_on {
                            let dep_job_id = dep.replace(['.', ' '], "-");
                            if seen.insert(dep_job_id.clone()) {
                                combined_needs.push(dep_job_id);
                            }
                        }
                    }
                    combined_needs.sort();

                    let synthetic_task = create_synthetic_aggregation_task(task_name, matrix_task);
                    let job = request.emitter.build_artifact_aggregation_job(
                        &synthetic_task,
                        request.ir,
                        &cuenv_github::workflow::ArtifactAggregationJobOptions {
                            environment: request.ctx.environment.as_ref(),
                            previous_jobs: &combined_needs,
                            project_path: request.ctx.project_path.as_deref(),
                            cuenv_setup: default_cuenv_setup.clone(),
                        },
                    );
                    if request.use_cuenv_bootstrap {
                        bootstrap_consumers.insert(job_id.clone());
                    }
                    jobs.insert(job_id, job);
                } else {
                    let ir_task = request.ctx.tasks.iter().find(|t| t.id == task_name);
                    let outputs = ir_task.map(|t| t.outputs.clone()).unwrap_or_default();
                    let synthetic_task =
                        create_synthetic_matrix_task(task_name, matrix_task, outputs);
                    let arch_runners = request
                        .ctx
                        .github_config
                        .runners
                        .as_ref()
                        .and_then(|r| r.arch.clone());

                    let expanded_jobs = request.emitter.build_matrix_jobs(
                        &synthetic_task,
                        request.ir,
                        &cuenv_github::workflow::MatrixJobOptions {
                            environment: request.ctx.environment.as_ref(),
                            arch_runners: arch_runners.as_ref(),
                            previous_jobs: &[],
                            project_path: request.ctx.project_path.as_deref(),
                            cuenv_artifacts_by_runner: cuenv_artifacts_by_runner.as_ref(),
                        },
                    );

                    for (id, job) in expanded_jobs {
                        artifact_source_jobs.insert(id.clone());
                        if request.use_cuenv_bootstrap {
                            bootstrap_consumers.insert(id.clone());
                        }
                        jobs.insert(id, job);
                    }
                }
            }
        }
    }

    let mut transitive_request = TransitiveDependencyJobRequest {
        jobs: &mut jobs,
        bootstrap_consumers: &mut bootstrap_consumers,
        processed_task_names: &processed_task_names,
        build_context: &build_context,
    };
    add_transitive_dependency_jobs(&mut transitive_request);
    BuiltPipelineJobs {
        jobs,
        bootstrap_consumers,
    }
}

fn add_transitive_dependency_jobs(request: &mut TransitiveDependencyJobRequest<'_>) {
    let build_context = request.build_context;
    for ir_task in &build_context.ctx.tasks {
        if ir_task.phase.is_some() || request.processed_task_names.contains(&ir_task.id) {
            continue;
        }

        let job_id = ir_task.id.replace(['.', ' '], "-");
        if request.jobs.contains_key(&job_id) {
            continue;
        }

        let execution = simple_job_execution(build_context.ctx, ir_task);
        let artifact_name = if execution == cuenv_github::workflow::TaskExecution::Orchestrated {
            build_context.default_cuenv_setup.artifact_name()
        } else {
            None
        };
        let mut job = build_context.emitter.build_simple_job(
            ir_task,
            build_context.ir,
            &simple_job_options(build_context.ctx, ir_task, artifact_name),
        );
        job.needs = ir_task
            .depends_on
            .iter()
            .map(|dep| dep.replace(['.', ' '], "-"))
            .collect();
        if build_context.use_cuenv_bootstrap
            && execution == cuenv_github::workflow::TaskExecution::Orchestrated
        {
            request.bootstrap_consumers.insert(job_id.clone());
        }
        request.jobs.insert(job_id, job);
    }
}

fn cuenv_artifacts_by_runner(
    ctx: &PipelineContext,
    emitter: &cuenv_github::workflow::GitHubActionsEmitter,
) -> HashMap<String, String> {
    let mut artifacts = HashMap::new();
    artifacts.insert(
        emitter.runner.clone(),
        cuenv_bootstrap_artifact_name(&cuenv_github::workflow::schema::RunsOn::Label(
            emitter.runner.clone(),
        )),
    );

    if let Some(arch_runners) = ctx
        .github_config
        .runners
        .as_ref()
        .and_then(|runners| runners.arch.as_ref())
    {
        for runner in arch_runners.values() {
            artifacts.entry(runner.clone()).or_insert_with(|| {
                cuenv_bootstrap_artifact_name(&cuenv_github::workflow::schema::RunsOn::Label(
                    runner.clone(),
                ))
            });
        }
    }

    artifacts
}

fn cuenv_setup_for_runner(
    runner: &str,
    artifacts_by_runner: Option<&HashMap<String, String>>,
) -> cuenv_github::workflow::CuenvSetup {
    artifacts_by_runner
        .and_then(|artifacts| artifacts.get(runner))
        .map_or(
            cuenv_github::workflow::CuenvSetup::BuildInJob,
            |artifact_name| cuenv_github::workflow::CuenvSetup::DownloadArtifact {
                artifact_name: artifact_name.clone(),
            },
        )
}

trait CuenvSetupExt {
    fn artifact_name(&self) -> Option<String>;
}

impl CuenvSetupExt for cuenv_github::workflow::CuenvSetup {
    fn artifact_name(&self) -> Option<String> {
        match self {
            Self::BuildInJob => None,
            Self::DownloadArtifact { artifact_name } => Some(artifact_name.clone()),
        }
    }
}

/// Create a synthetic IR Task for artifact aggregation from a `MatrixTask`.
fn create_synthetic_aggregation_task(
    task_name: &str,
    matrix_task: &cuenv_core::ci::MatrixTask,
) -> cuenv_ci::ir::Task {
    use cuenv_ci::ir::ArtifactDownload;

    let artifact_downloads = matrix_task
        .artifacts
        .as_ref()
        .map(|artifacts| {
            artifacts
                .iter()
                .map(|a| ArtifactDownload {
                    name: a.from.replace('.', "-"),
                    path: a.to.clone(),
                    filter: String::new(),
                })
                .collect()
        })
        .unwrap_or_default();

    let params: BTreeMap<String, String> = matrix_task
        .params
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();

    let mut task = synthetic_task_base(task_name);
    task.artifact_downloads = artifact_downloads;
    task.params = params;
    task
}

/// Create a synthetic IR Task for matrix expansion from a `MatrixTask`.
fn create_synthetic_matrix_task(
    task_name: &str,
    matrix_task: &cuenv_core::ci::MatrixTask,
    outputs: Vec<cuenv_ci::ir::OutputDeclaration>,
) -> cuenv_ci::ir::Task {
    use cuenv_ci::ir::MatrixConfig;

    let dimensions: BTreeMap<String, Vec<String>> = matrix_task
        .matrix
        .iter()
        .map(|(k, v)| {
            let mut sorted_values = v.clone();
            sorted_values.sort();
            (k.clone(), sorted_values)
        })
        .collect();

    let matrix = MatrixConfig {
        dimensions,
        exclude: vec![],
        include: vec![],
        max_parallel: 0,
        fail_fast: true,
    };

    let mut task = synthetic_task_base(task_name);
    task.outputs = outputs;
    task.matrix = Some(matrix);
    task
}

fn synthetic_task_base(task_name: &str) -> cuenv_ci::ir::Task {
    use cuenv_ci::ir::{CachePolicy, Task};

    Task {
        id: task_name.to_string(),
        runtime: None,
        command: vec![],
        shell: false,
        env: BTreeMap::new(),
        secrets: BTreeMap::new(),
        resources: None,
        concurrency_group: None,
        inputs: vec![],
        outputs: vec![],
        depends_on: vec![],
        cache_policy: CachePolicy::Normal,
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
