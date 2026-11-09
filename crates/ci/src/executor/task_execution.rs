//! CI task DAG execution and per-task IR runner setup.

use super::runner::{IRTaskRunner, TaskOutput};

use super::{ExecutorError, task_env, tools};
use crate::affected;
use crate::affected::ChangedInputs;
use crate::compiler::Compiler;
use crate::ir::CachePolicy;
use crate::report::{PipelineStatus, TaskReport, TaskStatus};
use cuenv_core::manifest::Project;

use cuenv_task_exec::graph_walk::{WalkCallbacks, WalkPolicy};
use cuenv_task_exec::{TaskGraph, TaskIndex};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(super) struct PipelineTasksRequest<'request> {
    pub(super) project_path: &'request Path,
    pub(super) project_display: &'request str,
    pub(super) config: &'request Project,
    pub(super) task_names: &'request [String],
    pub(super) environment: Option<&'request str>,
    pub(super) changed_files: &'request [PathBuf],
    pub(super) task_index: &'request TaskIndex,
    pub(super) cache_policy_override: Option<CachePolicy>,
    pub(super) hook_env: &'request BTreeMap<String, String>,
    pub(super) continue_on_error: bool,
    pub(super) max_parallel: usize,
}

pub(super) struct PipelineTaskResults {
    pub(super) reports: Vec<TaskReport>,
    pub(super) status: PipelineStatus,
    pub(super) errors: Vec<(String, cuenv_core::Error)>,
    pub(super) captures: HashMap<String, HashMap<String, String>>,
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

pub(super) async fn execute_pipeline_tasks(
    request: PipelineTasksRequest<'_>,
) -> PipelineTaskResults {
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
struct PipelineTaskRequest<'request> {
    task_name: &'request str,
    pipeline: PipelineTasksRequest<'request>,
}

async fn execute_pipeline_task(request: PipelineTaskRequest<'_>) -> PipelineTaskOutcome {
    let PipelineTaskRequest {
        task_name,
        pipeline,
    } = request;
    let inputs_matched = affected::matched_inputs_for_task(
        task_name,
        pipeline.config,
        ChangedInputs {
            files: pipeline.changed_files,
            project_root: pipeline.project_path,
        },
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
        max_parallel: pipeline.max_parallel,
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

struct TaskOutputOutcomeRequest<'request> {
    task_name: &'request str,
    project_display: &'request str,
    task_index: &'request TaskIndex,
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
    let error = cuenv_core::Error::task_failed(
        task_name,
        output.exit_code,
        cuenv_core::tasks::TaskFailureOutput {
            stdout: &output.stdout,
            stderr: &output.stderr,
        },
    );
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
        .map(|task| {
            cuenv_task_exec::captures::resolve_captures(
                &task.captures,
                &output.stdout,
                &output.stderr,
            )
        })
        .unwrap_or_default()
}

fn task_cache_key(output: &TaskOutput) -> String {
    if output.from_cache {
        format!("cached:{}", output.task_id)
    } else {
        output.task_id.clone()
    }
}

struct TaskDagOptions<'graph> {
    config: &'graph Project,
    task_name: &'graph str,
    project_root: &'graph Path,
    cache_policy_override: Option<CachePolicy>,
    environment: Option<&'graph str>,
    hook_env: &'graph BTreeMap<String, String>,
    continue_on_error: bool,
    max_parallel: usize,
}

async fn execute_task_with_deps(
    opts: TaskDagOptions<'_>,
) -> std::result::Result<TaskOutput, ExecutorError> {
    let TaskDagOptions {
        config,
        task_name,
        project_root,
        cache_policy_override,
        environment,
        hook_env,
        continue_on_error,
        max_parallel,
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
        .parallel_groups()
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

    let policy = task_walk_policy(WalkPolicy {
        max_parallel,
        continue_on_error,
    });
    let summary = cuenv_task_exec::graph_walk::walk_parallel_graph(
        graph.inner(),
        policy,
        WalkCallbacks {
            prepare_node: cuenv_task_exec::graph_walk::passthrough_prepare::<_, _, ExecutorError>,
            run_node: {
                let config = Arc::clone(&config);
                let project_root = Arc::clone(&project_root);
                let hook_env = Arc::clone(&hook_env);
                move |node: cuenv_task_graph::GraphNode<cuenv_core::tasks::Task>| {
                    let config = Arc::clone(&config);
                    let project_root = Arc::clone(&project_root);
                    let environment = environment.clone();
                    let hook_env = Arc::clone(&hook_env);
                    async move {
                        compile_and_execute_ir(IrExecutionRequest {
                            config: config.as_ref(),
                            task_name: &node.name,
                            project_root: project_root.as_ref(),
                            cache_policy_override,
                            environment: environment.as_deref(),
                            hook_env: hook_env.as_ref(),
                        })
                        .await
                    }
                }
            },
            from_join_error: |err: tokio::task::JoinError| {
                ExecutorError::Compilation(format!("CI orchestrator panic: {err}"))
            },
        },
    )
    .await?;

    let mut outputs: HashMap<String, TaskOutput> = summary.outcomes.into_iter().collect();

    if summary.failed > 0
        && let Some(failed) = outputs.values().find(|o| !o.success).cloned()
    {
        return Ok(failed);
    }

    outputs
        .remove(&canonical_name)
        .ok_or_else(|| ExecutorError::Compilation("No tasks to execute".into()))
}

fn task_walk_policy(policy: WalkPolicy) -> WalkPolicy {
    WalkPolicy {
        max_parallel: policy.max_parallel.max(1),
        ..policy
    }
}

/// The project environment variables a CI task sees: the base `env` merged
/// with the selected environment (`environment`, else `CUENV_ENVIRONMENT`).
pub(super) fn project_env_vars(
    config: &Project,
    environment: Option<&str>,
) -> HashMap<String, cuenv_core::environment::EnvValue> {
    let env_name = environment
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("CUENV_ENVIRONMENT")
                .ok()
                .filter(|name| !name.is_empty())
        });
    config
        .env
        .as_ref()
        .map(|env| match env_name.as_deref() {
            Some(name) => env.for_environment(name),
            None => env.base.clone(),
        })
        .unwrap_or_default()
}

struct IrExecutionRequest<'request> {
    config: &'request Project,
    task_name: &'request str,
    project_root: &'request Path,
    cache_policy_override: Option<CachePolicy>,
    environment: Option<&'request str>,
    hook_env: &'request BTreeMap<String, String>,
}

async fn compile_and_execute_ir(
    request: IrExecutionRequest<'_>,
) -> std::result::Result<TaskOutput, ExecutorError> {
    let IrExecutionRequest {
        config,
        task_name,
        project_root,
        cache_policy_override,
        environment,
        hook_env,
    } = request;
    let start = std::time::Instant::now();

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

    let project_env_vars = project_env_vars(config, environment);
    let (resolved_env, secrets) =
        cuenv_core::environment::Environment::resolve_for_task_with_secrets(
            task_name,
            &project_env_vars,
        )
        .await
        .map_err(|e| ExecutorError::Compilation(format!("Secret resolution failed: {e}")))?;

    cuenv_events::register_secrets(secrets);

    let runner = IRTaskRunner::new(
        project_root.to_path_buf(),
        cuenv_core::OutputCapture::Capture,
    );
    let mut combined_stdout = String::new();
    let mut combined_stderr = String::new();
    let mut all_success = true;
    let mut last_exit_code = 0;

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

    tools::ensure_tools_downloaded(project_root).await?;
    let activation_steps = tools::resolve_tool_activation_steps(project_root)?;
    if !activation_steps.is_empty() {
        tracing::debug!(
            steps = activation_steps.len(),
            "Applying configured tool activation operations for CI task execution"
        );
    }

    for ir_task in &ir.tasks {
        let mut env: BTreeMap<String, String> = hook_env.clone();
        for (key, value) in &runtime_env {
            env.insert(key.clone(), value.clone());
        }
        for (key, value) in &resolved_env {
            env.insert(key.clone(), value.clone());
        }
        task_env::apply_task_env(&mut env, &ir_task.env);

        tools::apply_tool_activation_steps(&mut env, &activation_steps);
        ensure_home_env(&mut env);

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
            break;
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

fn ensure_home_env(env: &mut BTreeMap<String, String>) {
    if !env.contains_key("HOME")
        && let Ok(home) = std::env::var("HOME")
    {
        env.insert("HOME".to_string(), home);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_walk_policy_uses_requested_parallelism() {
        let policy = task_walk_policy(WalkPolicy {
            max_parallel: 8,
            continue_on_error: true,
        });

        assert_eq!(policy.max_parallel, 8);
        assert!(policy.continue_on_error);
    }

    #[test]
    fn task_walk_policy_clamps_zero_parallelism() {
        let policy = task_walk_policy(WalkPolicy {
            max_parallel: 0,
            continue_on_error: false,
        });

        assert_eq!(policy.max_parallel, 1);
        assert!(!policy.continue_on_error);
    }
}
