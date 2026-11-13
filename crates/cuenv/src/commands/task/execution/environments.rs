//! The environment of each task of a run.
//!
//! A task receives the environment of the project it belongs to: that
//! project's runtime, tools and `env`, selected by that project's contexts
//! and policies. A task of another project, run as a dependency, receives
//! its own project's, and never the variables or secrets of the project the
//! run was asked for.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use cuenv_core::Result;
use cuenv_core::context::{TaskEnv, TaskEnvKey, TaskSelection, TaskSelectionOptions, TaskSubject};
use cuenv_core::environment::{EnvValue, Environment};
use cuenv_core::manifest::{Project, Runtime};
use cuenv_core::tasks::Task;
use cuenv_task_exec::{
    ExternalTaskName, ScopeProject, TaskEnvironmentFuture, TaskEnvironments, TaskScope,
};
use cuenv_tool_runtime::ResolvedToolActivationStep;
use tokio::sync::{Mutex, OnceCell};

/// An environment that is built once, by whichever task needs it first.
type BuiltEnvironment = Arc<OnceCell<Environment>>;

/// The project whose tasks' environments are built.
#[derive(Clone, Copy)]
pub(super) struct ProjectEnvironmentSpec<'spec> {
    pub(super) manifest: &'spec Project,
    pub(super) project_root: &'spec Path,
    /// The selected environment (`-e`). It applies to every project of a
    /// run; a project that does not define it uses its base variables.
    pub(super) environment: Option<&'spec str>,
}

/// Builds the environments of one project's tasks from the project variables
/// each task receives (see [`TaskSelection`]). An environment is built the
/// first time a task needs it, so a secret no task of the graph may receive
/// is never resolved.
#[derive(Debug)]
pub(super) struct ProjectTaskEnvironments {
    /// The runtime's environment, before the project's variables.
    runtime_env: Environment,
    activation_steps: Vec<ResolvedToolActivationStep>,
    /// The project's `env` and contexts; `None` for a project without `env`.
    selection: Option<TaskSelection>,
    /// The environments built so far. Each is built once; building one does
    /// not hold up a task that needs another.
    resolved: Mutex<HashMap<TaskEnvKey, BuiltEnvironment>>,
}

impl ProjectTaskEnvironments {
    /// Validate the project's contexts, resolve its runtime and download its
    /// tools.
    pub(super) async fn prepare(spec: ProjectEnvironmentSpec<'_>) -> Result<Self> {
        let ProjectEnvironmentSpec {
            manifest,
            project_root,
            environment,
        } = spec;
        // The contexts are validated here, once, and not again for each task.
        let selection = manifest
            .env
            .clone()
            .map(|env| {
                TaskSelection::new(TaskSelectionOptions {
                    env,
                    definitions: manifest.contexts.clone(),
                    environment: environment.map(str::to_string),
                    directory: project_root.to_path_buf(),
                })
            })
            .transpose()?;
        Ok(Self {
            runtime_env: runtime_environment(manifest, project_root).await?,
            activation_steps: lockfile_activation_steps(manifest, project_root).await?,
            selection,
            resolved: Mutex::default(),
        })
    }

    /// The project variables one task receives; `None` for a project
    /// without `env`. `name` is the task's name within the project.
    pub(super) fn task_env(&self, name: &str, task: &Task) -> Result<Option<TaskEnv>> {
        self.selection
            .as_ref()
            .map(|selection| selection.for_task(TaskSubject::of(name, task)))
            .transpose()
    }

    /// The project variables of a name that is not one task, such as a
    /// group; `None` for a project without `env`.
    pub(super) fn group_env(&self, name: &str) -> Result<Option<TaskEnv>> {
        self.selection
            .as_ref()
            .map(|selection| selection.for_group(name))
            .transpose()
    }

    /// The environment with a task's project variables, built once for the
    /// tasks that select the same ones.
    pub(super) async fn environment(&self, task_env: Option<&TaskEnv>) -> Result<Environment> {
        let key = task_env
            .map(|task_env| task_env.key().clone())
            .unwrap_or_default();
        let cell = Arc::clone(self.resolved.lock().await.entry(key).or_default());
        let environment = cell.get_or_try_init(|| self.build(task_env)).await?;
        Ok(environment.clone())
    }

    /// The environment with a task's project variables; without any for a
    /// project that has no `env`.
    async fn build(&self, task_env: Option<&TaskEnv>) -> Result<Environment> {
        let mut environment = self.runtime_env.clone();

        let env_vars = task_env.map(TaskEnv::variables).unwrap_or_default();
        let (resolved, secrets) = Environment::resolve_all_with_secrets(&env_vars).await?;

        cuenv_events::register_secrets(secrets);

        for (variable, value) in resolved {
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

/// A project whose tasks run as dependencies. Nothing of it is resolved
/// until one of its tasks is about to run.
#[derive(Debug)]
struct ExternalProjectEnvironments {
    project: ScopeProject,
    environment: Option<String>,
    prepared: OnceCell<ProjectTaskEnvironments>,
}

/// The environment of each task of a run, by the project that owns it.
#[derive(Debug)]
pub(super) struct RunTaskEnvironments {
    /// The project the run was asked for.
    own: ProjectTaskEnvironments,
    /// The key of the requested task, whose environment every task of the
    /// run's own project runs with unless its own key differs.
    shared: TaskEnvKey,
    /// The projects whose tasks run as dependencies, by what their tasks
    /// are qualified with.
    external: BTreeMap<String, ExternalProjectEnvironments>,
}

/// What the environments of a run are built from.
pub(super) struct RunEnvironments<'run> {
    /// The environments of the project the run was asked for.
    pub(super) own: ProjectTaskEnvironments,
    /// The key of the requested task.
    pub(super) shared: TaskEnvKey,
    /// The tasks of the run, which say which other projects it reaches.
    pub(super) scope: &'run TaskScope,
    /// The selected environment (`-e`).
    pub(super) environment: Option<&'run str>,
}

impl RunTaskEnvironments {
    /// The environments of a run and of the other projects it reaches.
    pub(super) fn new(run: RunEnvironments<'_>) -> Self {
        let RunEnvironments {
            own,
            shared,
            scope,
            environment,
        } = run;
        let external = scope
            .external_projects()
            .map(|(key, project)| {
                (
                    key.to_string(),
                    ExternalProjectEnvironments {
                        project: project.clone(),
                        environment: environment.map(str::to_string),
                        prepared: OnceCell::new(),
                    },
                )
            })
            .collect();
        Self {
            own,
            shared,
            external,
        }
    }

    /// Whether any task runs with an environment other than the shared one.
    pub(super) fn differs_by_task(&self) -> bool {
        self.own.selection.is_some() || !self.external.is_empty()
    }

    /// The environment of a task of another project: that project's own.
    async fn external_environment(
        &self,
        external: ExternalTaskName<'_>,
        task: &Task,
    ) -> Result<Environment> {
        let Some(project) = self.external.get(external.project) else {
            return Err(cuenv_core::Error::configuration(format!(
                "Task '{}' belongs to project '{}', which this run has not loaded. It is not \
                 run with another project's environment",
                external.qualified(),
                external.project
            )));
        };
        let environments = project
            .prepared
            .get_or_try_init(|| {
                ProjectTaskEnvironments::prepare(ProjectEnvironmentSpec {
                    manifest: &project.project.manifest,
                    project_root: &project.project.root,
                    environment: project.environment.as_deref(),
                })
            })
            .await?;
        // The project's contexts and policies know the task by its own name.
        let task_env = environments.task_env(external.task, task)?;
        environments.environment(task_env.as_ref()).await
    }
}

impl TaskEnvironments for RunTaskEnvironments {
    fn environment_for<'a>(&'a self, name: &'a str, task: &'a Task) -> TaskEnvironmentFuture<'a> {
        Box::pin(async move {
            if let Some(external) = ExternalTaskName::parse(name) {
                return self.external_environment(external, task).await.map(Some);
            }
            match self.own.task_env(name, task)? {
                Some(task_env) if *task_env.key() != self.shared => {
                    self.own.environment(Some(&task_env)).await.map(Some)
                }
                _ => Ok(None),
            }
        })
    }
}

async fn runtime_environment(manifest: &Project, project_root: &Path) -> Result<Environment> {
    let mut runtime_env = Environment::new();
    let runtime_env_vars =
        cuenv_core::runtime::resolve_runtime_environment(project_root, manifest.runtime.as_ref())
            .await?;
    for (key, value) in runtime_env_vars {
        runtime_env.set(key, value);
    }
    Ok(runtime_env)
}

/// The lockfile's tool activation steps, after downloading the tools; none
/// unless the project uses the tools runtime.
async fn lockfile_activation_steps(
    manifest: &Project,
    project_root: &Path,
) -> Result<Vec<ResolvedToolActivationStep>> {
    if !matches!(manifest.runtime, Some(Runtime::Tools(_))) {
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
