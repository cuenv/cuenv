//! GitHub Actions matrix workflow emission.

use super::{
    PipelineContext, inject_cuenv_bootstrap_jobs, sanitize_workflow_name, simple_job_options,
};
use cuenv_core::Result;
use std::collections::{BTreeMap, HashSet};

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

    let mut jobs = build_pipeline_jobs(&expanded_tasks, ctx, &ir, &emitter);
    inject_cuenv_bootstrap_jobs(&mut jobs, &ir, &emitter);

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
fn build_pipeline_jobs(
    expanded_tasks: &[cuenv_core::ci::PipelineTask],
    ctx: &PipelineContext,
    ir: &cuenv_ci::ir::IntermediateRepresentation,
    emitter: &cuenv_github::workflow::GitHubActionsEmitter,
) -> indexmap::IndexMap<String, cuenv_github::workflow::schema::Job> {
    use indexmap::IndexMap;

    let mut jobs = IndexMap::new();
    let mut artifact_source_jobs: HashSet<String> = HashSet::new();
    let mut processed_task_names: HashSet<String> = HashSet::new();

    for pipeline_task in expanded_tasks {
        let task_name = pipeline_task.task_name();
        processed_task_names.insert(task_name.to_string());
        let job_id = task_name.replace(['.', ' '], "-");

        match pipeline_task {
            cuenv_core::ci::PipelineTask::Simple(_) | cuenv_core::ci::PipelineTask::Node(_) => {
                if let Some(ir_task) = ctx.tasks.iter().find(|t| t.id == task_name) {
                    let mut job =
                        emitter.build_simple_job(ir_task, ir, simple_job_options(ctx, ir_task));
                    job.needs = ir_task
                        .depends_on
                        .iter()
                        .map(|dep| dep.replace(['.', ' '], "-"))
                        .collect();
                    jobs.insert(job_id, job);
                }
            }
            cuenv_core::ci::PipelineTask::Matrix(matrix_task) => {
                if matrix_task.matrix.is_empty() {
                    let ir_task = ctx.tasks.iter().find(|t| t.id == task_name);
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
                    let job = emitter.build_artifact_aggregation_job(
                        &synthetic_task,
                        ir,
                        ctx.environment.as_ref(),
                        &combined_needs,
                        ctx.project_path.as_deref(),
                    );
                    jobs.insert(job_id, job);
                } else {
                    let ir_task = ctx.tasks.iter().find(|t| t.id == task_name);
                    let outputs = ir_task.map(|t| t.outputs.clone()).unwrap_or_default();
                    let synthetic_task =
                        create_synthetic_matrix_task(task_name, matrix_task, outputs);
                    let arch_runners = ctx
                        .github_config
                        .runners
                        .as_ref()
                        .and_then(|r| r.arch.clone());

                    let expanded_jobs = emitter.build_matrix_jobs(
                        &synthetic_task,
                        ir,
                        ctx.environment.as_ref(),
                        arch_runners.as_ref(),
                        &[],
                        ctx.project_path.as_deref(),
                    );

                    for (id, job) in expanded_jobs {
                        artifact_source_jobs.insert(id.clone());
                        jobs.insert(id, job);
                    }
                }
            }
        }
    }

    add_transitive_dependency_jobs(&mut jobs, &processed_task_names, ctx, ir, emitter);
    jobs
}

fn add_transitive_dependency_jobs(
    jobs: &mut indexmap::IndexMap<String, cuenv_github::workflow::schema::Job>,
    processed_task_names: &HashSet<String>,
    ctx: &PipelineContext,
    ir: &cuenv_ci::ir::IntermediateRepresentation,
    emitter: &cuenv_github::workflow::GitHubActionsEmitter,
) {
    for ir_task in &ctx.tasks {
        if ir_task.phase.is_some() || processed_task_names.contains(&ir_task.id) {
            continue;
        }

        let job_id = ir_task.id.replace(['.', ' '], "-");
        if jobs.contains_key(&job_id) {
            continue;
        }

        let mut job = emitter.build_simple_job(ir_task, ir, simple_job_options(ctx, ir_task));
        job.needs = ir_task
            .depends_on
            .iter()
            .map(|dep| dep.replace(['.', ' '], "-"))
            .collect();
        jobs.insert(job_id, job);
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
