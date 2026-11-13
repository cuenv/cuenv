//! CI task DAG execution and per-task IR runner setup.

use super::projects::{CiProjects, HookEnvironment, HookProject};
use super::runner::{IRTaskRunner, TaskOutput};

use super::{ExecutorError, task_env, tools};
use crate::affected;
use crate::affected::ChangedInputs;
use crate::compiler::Compiler;
use crate::ir::CachePolicy;
use crate::report::{PipelineStatus, TaskReport, TaskStatus};
use cuenv_core::context::{TaskEnv, TaskSelection, TaskSelectionOptions, TaskSubject};
use cuenv_core::environment::Environment;
use cuenv_core::manifest::Project;
use cuenv_core::tasks::{Input, Task};

use cuenv_task_exec::graph_walk::{WalkCallbacks, WalkPolicy};
use cuenv_task_exec::{
    ExecutorConfig, ProjectExecution, RootProject, ScopeProject, TaskExecutor, TaskIndex, TaskScope,
};
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
    pub(super) hook_env: &'request HookEnvironment,
    pub(super) continue_on_error: bool,
    pub(super) max_parallel: usize,
    /// The project's tasks and the tasks of the projects they depend on.
    pub(super) scope: &'request Arc<TaskScope>,
    /// Every project of the module, for the dependencies outside this one.
    pub(super) projects: &'request Arc<CiProjects>,
    /// The projects in scope by directory, whose hooks a project may inherit.
    pub(super) project_configs: &'request Arc<HashMap<PathBuf, Project>>,
}

/// What a pipeline's task scope is built from.
#[derive(Clone, Copy)]
pub(super) struct PipelineScope<'scope> {
    pub(super) project_path: &'scope Path,
    pub(super) config: &'scope Project,
    pub(super) task_index: &'scope TaskIndex,
    /// The pipeline tasks about to run.
    pub(super) task_names: &'scope [String],
    pub(super) projects: &'scope CiProjects,
}

/// The tasks a pipeline can reach: the project's own, and those of every
/// project the tasks about to run depend on, in or out of `--path`.
///
/// A project that cannot be loaded does not fail here. The pipeline task
/// that depends on it fails, with the reason, when its graph is built.
pub(super) fn pipeline_scope(pipeline: PipelineScope<'_>) -> TaskScope {
    let PipelineScope {
        project_path,
        config,
        task_index,
        task_names,
        projects,
    } = pipeline;
    let mut scope = TaskScope::new(RootProject {
        project: ScopeProject {
            root: project_path.to_path_buf(),
            module_root: projects.module_root().to_path_buf(),
            manifest: Arc::new(config.clone()),
        },
        tasks: task_index.to_tasks(),
        output_ref_deps: Vec::new(),
    });
    let selected: Vec<&str> = task_names
        .iter()
        .filter_map(|name| task_index.resolve(name).ok())
        .map(|entry| entry.name.as_str())
        .collect();
    scope.reach(selected, projects);
    scope
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
    // Changed files are relative to the root of the checkout, so the project
    // is placed there before its inputs are compared with them.
    let inputs_matched = affected::CheckoutFrame::around(pipeline.project_path)
        .place(pipeline.project_path)
        .map(|project_root| {
            affected::matched_inputs_for_task(
                task_name,
                pipeline.config,
                ChangedInputs {
                    files: pipeline.changed_files,
                    project_root: &project_root,
                },
            )
        })
        .unwrap_or_default();
    let outputs = task_outputs(pipeline.task_index, task_name);

    cuenv_events::emit_ci_task_executing!(pipeline.project_display, task_name);
    let task_start = std::time::Instant::now();

    let result = execute_task_with_deps(request).await;

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

async fn execute_task_with_deps(
    request: PipelineTaskRequest<'_>,
) -> std::result::Result<TaskOutput, ExecutorError> {
    let PipelineTaskRequest {
        task_name,
        pipeline,
    } = request;
    let compilation = |error: cuenv_core::Error| ExecutorError::Compilation(error.to_string());

    let entry = pipeline
        .task_index
        .resolve(task_name)
        .map_err(compilation)?;
    let canonical_name = entry.name.clone();

    // The graph follows dependencies into other projects. A task there runs
    // in its own project, not in this one.
    let graph = pipeline
        .scope
        .graph([canonical_name.as_str()])
        .map_err(compilation)?;

    let parallel_groups = graph.parallel_groups().map_err(compilation)?;

    tracing::info!(
        task = task_name,
        canonical = %canonical_name,
        continue_on_error = pipeline.continue_on_error,
        execution_order = ?parallel_groups
            .iter()
            .map(|g| g.iter().map(|n| n.name.clone()).collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        "Resolved task dependencies"
    );
    drop(parallel_groups);

    let run = Arc::new(PipelineRun {
        project_root: pipeline.project_path.to_path_buf(),
        hook_env: Arc::clone(pipeline.hook_env),
        scope: Arc::clone(pipeline.scope),
        projects: Arc::clone(pipeline.projects),
        project_configs: Arc::clone(pipeline.project_configs),
        cache_policy_override: pipeline.cache_policy_override,
        environment: pipeline.environment.map(str::to_string),
    });

    let policy = task_walk_policy(WalkPolicy {
        max_parallel: pipeline.max_parallel,
        continue_on_error: pipeline.continue_on_error,
    });
    let summary = cuenv_task_exec::graph_walk::walk_parallel_graph(
        graph.inner(),
        policy,
        WalkCallbacks {
            prepare_node: cuenv_task_exec::graph_walk::passthrough_prepare::<_, _, ExecutorError>,
            run_node: {
                let run = Arc::clone(&run);
                move |node: cuenv_task_graph::GraphNode<Task>| {
                    let run = Arc::clone(&run);
                    async move { run.execute(&node).await }
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

/// What the tasks of one pipeline task's graph run with.
struct PipelineRun {
    /// Directory of the project the pipeline belongs to.
    project_root: PathBuf,
    /// The environment that project's hooks provide.
    hook_env: HookEnvironment,
    scope: Arc<TaskScope>,
    projects: Arc<CiProjects>,
    project_configs: Arc<HashMap<PathBuf, Project>>,
    cache_policy_override: Option<CachePolicy>,
    /// The environment the run selects. It applies to every project; one
    /// that does not define it uses its base variables.
    environment: Option<String>,
}

impl PipelineRun {
    /// Run one node of the graph in the project that owns it.
    async fn execute(
        &self,
        node: &cuenv_task_graph::GraphNode<Task>,
    ) -> std::result::Result<TaskOutput, ExecutorError> {
        let owner = self.scope.owner(&node.name).ok_or_else(|| {
            ExecutorError::Compilation(format!(
                "Task '{}' belongs to a project this run has not loaded",
                node.name
            ))
        })?;
        // A task of another project receives what its own hooks and `env`
        // provide, built the first time one of its tasks runs.
        let (project_root, hook_env) = match owner.key {
            None => (self.project_root.as_path(), Arc::clone(&self.hook_env)),
            Some(_) => (
                owner.project.root.as_path(),
                self.projects
                    .hook_environment(HookProject {
                        root: &owner.project.root,
                        config: &owner.project.manifest,
                        ancestor_configs: &self.project_configs,
                    })
                    .await
                    .map_err(|error| ExecutorError::Compilation(error.to_string()))?,
            ),
        };
        compile_and_execute_ir(IrExecutionRequest {
            config: &owner.project.manifest,
            task_name: &node.name,
            task: &node.task,
            subject: owner.task,
            project_root,
            cache_policy_override: self.cache_policy_override,
            environment: self.environment.as_deref(),
            hook_env: &hook_env,
            staging: Staging {
                project_root: &owner.project.root,
                module_root: &owner.project.module_root,
                workspace_root: self.projects.workspace_root(),
                project_key: owner.key,
            },
        })
        .await
    }
}

fn task_walk_policy(policy: WalkPolicy) -> WalkPolicy {
    WalkPolicy {
        max_parallel: policy.max_parallel.max(1),
        ..policy
    }
}

/// The environment a CI run selects: `environment`, else `CUENV_ENVIRONMENT`.
fn selected_environment(environment: Option<&str>) -> Option<String> {
    environment
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("CUENV_ENVIRONMENT")
                .ok()
                .filter(|name| !name.is_empty())
        })
}

/// A pipeline whose tasks are about to run.
#[derive(Clone, Copy)]
pub(super) struct PlannedPipeline<'plan> {
    pub(super) project_path: &'plan Path,
    pub(super) config: &'plan Project,
    /// The pipeline tasks about to run.
    pub(super) task_names: &'plan [String],
    pub(super) environment: Option<&'plan str>,
    pub(super) projects: &'plan CiProjects,
}

/// Every value the tasks of a planned pipeline could see: those of the
/// pipeline's own project, and those of each project its tasks depend on,
/// whose tasks run with their own project's environment.
pub(super) fn pipeline_env_values(
    plan: PlannedPipeline<'_>,
) -> Vec<cuenv_core::environment::EnvValue> {
    let PlannedPipeline {
        project_path,
        config,
        task_names,
        environment,
        projects,
    } = plan;
    let mut values = project_env_values_in_any_context(config, environment);
    // A project whose tasks cannot be indexed fails when its pipeline runs.
    if let Ok(task_index) = TaskIndex::build(&config.tasks) {
        let scope = pipeline_scope(PipelineScope {
            project_path,
            config,
            task_index: &task_index,
            task_names,
            projects,
        });
        for (_, project) in scope.external_projects() {
            values.extend(project_env_values_in_any_context(
                &project.manifest,
                environment,
            ));
        }
    }
    values
}

/// Every value a project's tasks could see: the selected environment and
/// each context's variables, whichever contexts turn out to be active. Used
/// to log in to secret providers before any task runs. Two contexts may set
/// one variable from different providers, so the values are not merged by
/// name.
fn project_env_values_in_any_context(
    config: &Project,
    environment: Option<&str>,
) -> Vec<cuenv_core::environment::EnvValue> {
    let env_name = selected_environment(environment);
    let Some(env) = &config.env else {
        return Vec::new();
    };
    let selected = env.select(cuenv_core::environment::EnvSelection {
        environment: env_name.as_deref(),
        contexts: &[],
    });
    let in_contexts = env.context.iter().flatten().flat_map(|(_, context)| {
        let overrides = env_name.as_deref().and_then(|name| {
            context
                .environment
                .as_ref()
                .and_then(|environments| environments.get(name))
        });
        context
            .base
            .values()
            .chain(overrides.into_iter().flat_map(HashMap::values))
    });
    selected.values().chain(in_contexts).cloned().collect()
}

/// Where a task that maps inputs is staged.
#[derive(Clone, Copy)]
struct Staging<'staging> {
    /// The task's project directory, in the canonical spelling that inputs
    /// are hashed in.
    project_root: &'staging Path,
    /// Root of the CUE module of the task's project.
    module_root: &'staging Path,
    /// The directory inputs are hashed below.
    workspace_root: &'staging Path,
    /// What the task's project is qualified with; `None` for the project
    /// the pipeline belongs to.
    project_key: Option<&'staging str>,
}

struct IrExecutionRequest<'request> {
    /// The project that owns the task.
    config: &'request Project,
    /// The task's name in the graph, qualified when it is another project's.
    task_name: &'request str,
    task: &'request Task,
    /// The task's name within its project, which is what the project's
    /// contexts and policies match.
    subject: &'request str,
    project_root: &'request Path,
    cache_policy_override: Option<CachePolicy>,
    environment: Option<&'request str>,
    hook_env: &'request BTreeMap<String, String>,
    staging: Staging<'request>,
}

async fn compile_and_execute_ir(
    request: IrExecutionRequest<'_>,
) -> std::result::Result<TaskOutput, ExecutorError> {
    let IrExecutionRequest {
        config,
        task_name,
        task,
        subject,
        project_root,
        cache_policy_override,
        environment,
        hook_env,
        staging,
    } = request;
    let start = std::time::Instant::now();

    let options = crate::compiler::CompilerOptions {
        project_root: Some(project_root.to_path_buf()),
        ..Default::default()
    };
    let compiler = Compiler::with_options(config.clone(), options);
    let ir_task = compiler
        .compile_single_task(task_name, task)
        .map_err(|e| ExecutorError::Compilation(e.to_string()))?;

    let env_name = selected_environment(environment);

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

    // A named context can select variables for one task and not for another,
    // and so can a variable's policies, so the project variables are
    // selected for the task that receives them, by its name in its project.
    let task_env = config
        .env
        .clone()
        .map(|env| {
            TaskSelection::new(TaskSelectionOptions {
                env,
                definitions: config.contexts.clone(),
                environment: env_name.clone(),
                directory: project_root.to_path_buf(),
            })
        })
        .transpose()
        .and_then(|selection| {
            selection
                .map(|selection| selection.for_task(TaskSubject::of(subject, task)))
                .transpose()
        })
        .map_err(|e| ExecutorError::Compilation(e.to_string()))?;
    let variables = task_env
        .as_ref()
        .map(TaskEnv::variables)
        .unwrap_or_default();
    let (resolved, secrets) =
        cuenv_core::environment::Environment::resolve_all_with_secrets(&variables)
            .await
            .map_err(|e| ExecutorError::Compilation(format!("Secret resolution failed: {e}")))?;
    cuenv_events::register_secrets(secrets);

    let mut env: BTreeMap<String, String> = hook_env.clone();
    env.extend(runtime_env);
    env.extend(resolved);
    // The IR keeps only a task's string variables. Its secrets, numbers and
    // booleans are resolved from the project task, as `cuenv task` does, so
    // a task-level secret reaches the task in CI too.
    let (resolved, secrets) = cuenv_task_exec::env::resolve_task_env(subject, &task.env)
        .await
        .map_err(|e| ExecutorError::Compilation(format!("Secret resolution failed: {e}")))?;
    cuenv_events::register_secrets(secrets);
    env.extend(resolved);
    task_env::apply_task_env(&mut env, &ir_task.env);

    tools::apply_tool_activation_steps(&mut env, &activation_steps);
    ensure_home_env(&mut env);

    let mut task_to_run = ir_task;
    if let Some(policy) = cache_policy_override {
        task_to_run.cache_policy = policy;
    }

    // A mapped input is read where the mapping puts it, and only an
    // execution root can put it there. Every other task runs in the
    // checkout, where an output is already where its task wrote it.
    let output = if relocates_an_input(task) {
        execute_staged(StagedTask {
            task_name,
            task,
            env,
            staging,
        })
        .await?
    } else {
        IRTaskRunner::new(
            project_root.to_path_buf(),
            cuenv_core::OutputCapture::Capture,
        )
        .execute(&task_to_run, env)
        .await?
    };

    let duration = start.elapsed();
    let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);

    Ok(TaskOutput {
        task_id: task_name.to_string(),
        exit_code: output.exit_code,
        stdout: output.stdout,
        stderr: output.stderr,
        success: output.success,
        from_cache: false,
        duration_ms,
        captures: HashMap::new(),
    })
}

/// Whether a task reads a file somewhere other than where it was produced.
fn relocates_an_input(task: &Task) -> bool {
    task.inputs.iter().any(|input| match input {
        Input::Mapped(mapping) => mapping.relocates(),
        Input::Project(reference) => !reference.map.is_empty(),
        Input::Path(_) | Input::Task(_) => false,
    })
}

struct StagedTask<'staged> {
    task_name: &'staged str,
    task: &'staged Task,
    /// The environment the task would run with in the checkout.
    env: BTreeMap<String, String>,
    staging: Staging<'staged>,
}

/// Run a task that maps inputs as `cuenv task` runs it: in an execution root
/// holding its declared inputs, each mapped file at its `to` path, with its
/// declared outputs copied back into the project.
///
/// A task that opted out of the directory sandbox has nowhere to stage a
/// mapping, and is refused with that explanation.
async fn execute_staged(staged: StagedTask<'_>) -> std::result::Result<TaskOutput, ExecutorError> {
    let StagedTask {
        task_name,
        task,
        env,
        staging,
    } = staged;
    let mut environment = Environment::new();
    for (name, value) in env {
        environment.set(name, value);
    }
    // A task in the checkout finds its programs on the host's `PATH` when
    // nothing declares one. So does this one.
    if environment.get("PATH").is_none()
        && let Ok(path) = std::env::var("PATH")
    {
        environment.set("PATH".to_string(), path);
    }

    let execution = ProjectExecution {
        project_root: staging.project_root.to_path_buf(),
        cue_module_root: Some(staging.module_root.to_path_buf()),
        cache: None,
    };
    let executor = TaskExecutor::new(ExecutorConfig {
        capture_output: cuenv_core::OutputCapture::Capture,
        max_parallel: 1,
        continue_on_error: false,
        environment,
        task_environments: None,
        working_dir: None,
        project_root: execution.project_root.clone(),
        cue_module_root: execution.cue_module_root.clone(),
        cache: None,
        sandbox_hasher_root: Some(staging.workspace_root.to_path_buf()),
        sandbox_project_roots: BTreeMap::new(),
        external_projects: staging
            .project_key
            .map(|key| (key.to_string(), execution))
            .into_iter()
            .collect(),
    });
    let result = executor
        .execute_task(task_name, task)
        .await
        .map_err(|error| ExecutorError::Compilation(error.to_string()))?;

    Ok(TaskOutput {
        task_id: task_name.to_string(),
        exit_code: result.exit_code.unwrap_or(-1),
        stdout: result.stdout,
        stderr: result.stderr,
        success: result.success,
        from_cache: false,
        duration_ms: 0,
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

    /// Run one task of `project` in `directory`, as a node of a graph would.
    async fn run_in_project(
        project: &Project,
        directory: &Path,
        name: &str,
    ) -> std::result::Result<TaskOutput, ExecutorError> {
        let task = project.tasks[name].as_task().expect("a task");
        compile_and_execute_ir(IrExecutionRequest {
            config: project,
            task_name: name,
            task,
            subject: name,
            project_root: directory,
            cache_policy_override: None,
            environment: None,
            hook_env: &BTreeMap::new(),
            staging: Staging {
                project_root: directory,
                module_root: directory,
                workspace_root: directory,
                project_key: None,
            },
        })
        .await
    }

    fn project_with_task(name: &str, task: serde_json::Value) -> Project {
        let task: Task = serde_json::from_value(task).expect("task");
        let mut project = Project::new("test");
        project.tasks.insert(
            name.to_string(),
            cuenv_core::tasks::TaskNode::Task(Box::new(task)),
        );
        project
    }

    #[tokio::test]
    async fn a_task_level_secret_reaches_the_task() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let project = project_with_task(
            "probe",
            serde_json::json!({
                "command": "sh",
                "args": ["-c", "test \"$TOKEN\" = hello && test \"$PLAIN\" = yes"],
                "hermetic": false,
                "env": {
                    "TOKEN": {"resolver": "exec", "command": "echo", "args": ["hello"]},
                    "PLAIN": "yes",
                },
            }),
        );

        let output = run_in_project(&project, directory.path(), "probe")
            .await
            .expect("task runs");

        assert!(output.success, "{}", output.stderr);
    }

    #[tokio::test]
    async fn a_task_that_maps_an_input_reads_it_where_the_mapping_puts_it() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().expect("canonical");
        std::fs::create_dir_all(root.join("built")).expect("directory");
        std::fs::write(root.join("built/one.md"), "one\n").expect("file");
        let mut project = project_with_task(
            "consume",
            serde_json::json!({
                "command": "sh",
                "args": ["-c", "cp vendor/one.md seen.md"],
                "outputs": ["seen.md"],
            }),
        );
        if let Some(cuenv_core::tasks::TaskNode::Task(task)) = project.tasks.get_mut("consume") {
            task.inputs = vec![Input::Mapped(cuenv_core::tasks::MappedInput {
                source: "built/one.md".to_string(),
                destination: "vendor/one.md".to_string(),
                producer_task: Some("produce".to_string()),
            })];
        }

        let output = run_in_project(&project, &root, "consume")
            .await
            .expect("task runs");

        assert!(output.success, "{}", output.stderr);
        assert_eq!(
            std::fs::read_to_string(root.join("seen.md")).expect("the declared output"),
            "one\n"
        );
        assert!(
            !root.join("vendor").exists(),
            "the mapped file is staged in an execution root, not in the checkout"
        );
    }

    #[tokio::test]
    async fn a_mapping_no_execution_root_can_honour_is_an_error() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().expect("canonical");
        std::fs::create_dir_all(root.join("built")).expect("directory");
        std::fs::write(root.join("built/one.md"), "one\n").expect("file");
        let mut project = project_with_task(
            "consume",
            serde_json::json!({
                "command": "sh",
                "args": ["-c", "true"],
                "hermetic": {"sandbox": "none"},
            }),
        );
        if let Some(cuenv_core::tasks::TaskNode::Task(task)) = project.tasks.get_mut("consume") {
            task.inputs = vec![Input::Mapped(cuenv_core::tasks::MappedInput {
                source: "built/one.md".to_string(),
                destination: "vendor/one.md".to_string(),
                producer_task: Some("produce".to_string()),
            })];
        }

        let error = run_in_project(&project, &root, "consume")
            .await
            .expect_err("nothing would stage the mapping");

        assert!(
            error.to_string().contains("nothing is staged"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn a_step_of_a_sequence_runs_under_its_graph_name() {
        // A graph names the steps of a sequence `name[index]`. The step is
        // compiled from the node itself, not looked up by that name.
        let directory = tempfile::tempdir().expect("temporary directory");
        let project = project_with_task(
            "step",
            serde_json::json!({"command": "sh", "args": ["-c", "echo ran"], "hermetic": false}),
        );
        let task = project.tasks["step"].as_task().expect("a task");

        let output = compile_and_execute_ir(IrExecutionRequest {
            config: &project,
            task_name: "release[0]",
            task,
            subject: "release[0]",
            project_root: directory.path(),
            cache_policy_override: None,
            environment: None,
            hook_env: &BTreeMap::new(),
            staging: Staging {
                project_root: directory.path(),
                module_root: directory.path(),
                workspace_root: directory.path(),
                project_key: None,
            },
        })
        .await
        .expect("task runs");

        assert!(output.success, "{}", output.stderr);
        assert_eq!(output.task_id, "release[0]");
        assert_eq!(output.stdout.trim(), "ran");
    }

    #[test]
    fn a_pipeline_may_resolve_the_secrets_of_the_projects_it_depends_on() {
        // Providers log in before any task runs, for every value a task of
        // the run may resolve: also those of a dependency's own project.
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().canonicalize().expect("canonical");
        let project = |name: &str, value: &str, tasks: serde_json::Value| {
            std::fs::create_dir_all(root.join(name)).expect("directory");
            let project: Project = serde_json::from_value(serde_json::json!({
                "name": name,
                "env": {"VALUE": value},
                "tasks": tasks,
            }))
            .expect("project");
            (root.join(name), project)
        };
        let app = project(
            "app",
            "of-app",
            serde_json::json!({
                "build": {"command": "true", "dependsOn": ["#content:articles"]},
                "lint": {"command": "true"},
            }),
        );
        let all = [
            app.clone(),
            project(
                "content",
                "of-content",
                serde_json::json!({"articles": {"command": "true"}}),
            ),
            project(
                "docs",
                "of-docs",
                serde_json::json!({"site": {"command": "true"}}),
            ),
        ];
        let projects = CiProjects::new(&root, &all);
        let values_for = |task: &str| {
            let mut values: Vec<String> = pipeline_env_values(PlannedPipeline {
                project_path: &app.0,
                config: &app.1,
                task_names: &[task.to_string()],
                environment: None,
                projects: &projects,
            })
            .iter()
            .map(cuenv_core::environment::EnvValue::to_string_value)
            .collect();
            values.sort();
            values
        };

        assert_eq!(values_for("build"), ["of-app", "of-content"]);
        assert_eq!(values_for("lint"), ["of-app"]);
    }

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
