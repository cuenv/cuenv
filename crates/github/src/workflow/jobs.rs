//! GitHub Actions job construction helpers.

use crate::workflow::emitter::GitHubActionsEmitter;
use crate::workflow::schema::{Job, RunsOn, Step};
use crate::workflow::stage_renderer::transform_secret_ref;
use cuenv_ci::ir::{BuildStage, IntermediateRepresentation, OutputType, Task};
use indexmap::IndexMap;
use std::collections::{HashMap, HashSet};

const CUENV_BOOTSTRAP_ARTIFACT_DIR: &str = "${{ runner.temp }}/cuenv-bootstrap";
const CUENV_BOOTSTRAP_ARTIFACT_PATH: &str = "$RUNNER_TEMP/cuenv-bootstrap";

/// How a regular GitHub Actions job should execute its main task.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TaskExecution {
    /// Run the task through `cuenv task ... --skip-dependencies`.
    #[default]
    Orchestrated,
    /// Run the task's IR command directly as a GitHub Actions step.
    Direct,
}

/// How a job should satisfy its need for the `cuenv` binary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CuenvSetup {
    /// Render the normal `cuenv.setup` phase task inside the job.
    #[default]
    BuildInJob,
    /// Download a `cuenv` binary produced by an earlier bootstrap job.
    DownloadArtifact {
        /// Name of the GitHub Actions artifact containing the `cuenv` binary.
        artifact_name: String,
    },
}

/// Options for building a simple GitHub Actions job.
#[derive(Debug, Clone, Default)]
pub struct SimpleJobOptions<'a> {
    /// Optional environment name for orchestrated execution.
    pub environment: Option<&'a String>,
    /// Optional working directory for monorepo jobs.
    pub project_path: Option<&'a str>,
    /// Whether to run the task through cuenv or directly.
    pub execution: TaskExecution,
    /// How the job should obtain the `cuenv` binary for orchestrated execution.
    pub cuenv_setup: CuenvSetup,
}

impl<'a> SimpleJobOptions<'a> {
    /// Build options for a simple job that should execute via `cuenv task`.
    #[must_use]
    pub fn orchestrated(environment: Option<&'a String>, project_path: Option<&'a str>) -> Self {
        Self {
            environment,
            project_path,
            execution: TaskExecution::Orchestrated,
            cuenv_setup: CuenvSetup::BuildInJob,
        }
    }

    /// Build options for a simple job that should execute via a bootstrap artifact.
    #[must_use]
    pub fn orchestrated_with_cuenv_artifact(
        environment: Option<&'a String>,
        project_path: Option<&'a str>,
        artifact_name: String,
    ) -> Self {
        Self {
            environment,
            project_path,
            execution: TaskExecution::Orchestrated,
            cuenv_setup: CuenvSetup::DownloadArtifact { artifact_name },
        }
    }

    /// Build options for a simple job that should run the IR command directly.
    #[must_use]
    pub fn direct(project_path: Option<&'a str>) -> Self {
        Self {
            environment: None,
            project_path,
            execution: TaskExecution::Direct,
            cuenv_setup: CuenvSetup::BuildInJob,
        }
    }
}

/// Options for building an artifact aggregation job.
#[derive(Debug, Clone, Default)]
pub struct ArtifactAggregationJobOptions<'a> {
    /// Optional environment name for orchestrated execution.
    pub environment: Option<&'a String>,
    /// Jobs whose artifacts should be downloaded before the task runs.
    pub previous_jobs: &'a [String],
    /// Optional working directory for monorepo jobs.
    pub project_path: Option<&'a str>,
    /// How the job should obtain the `cuenv` binary.
    pub cuenv_setup: CuenvSetup,
}

/// Options for building matrix-expanded jobs.
#[derive(Debug, Clone, Default)]
pub struct MatrixJobOptions<'a> {
    /// Optional environment name for orchestrated execution.
    pub environment: Option<&'a String>,
    /// Optional per-architecture runner labels.
    pub arch_runners: Option<&'a HashMap<String, String>>,
    /// Jobs that must complete before each matrix job.
    pub previous_jobs: &'a [String],
    /// Optional working directory for monorepo jobs.
    pub project_path: Option<&'a str>,
    /// Optional mapping from runner label to bootstrap artifact name.
    pub cuenv_artifacts_by_runner: Option<&'a HashMap<String, String>>,
}

/// Options for building a cuenv bootstrap job.
#[derive(Debug, Clone)]
pub struct CuenvBootstrapJobOptions<'a> {
    /// Runner for the bootstrap job.
    pub runs_on: RunsOn,
    /// Display name for the bootstrap job.
    pub name: &'a str,
    /// Artifact name used when uploading the built cuenv binary.
    pub artifact_name: &'a str,
}

impl GitHubActionsEmitter {
    fn direct_execution_skipped_setup_task_ids(
        ir: &IntermediateRepresentation,
        execution: TaskExecution,
    ) -> HashSet<String> {
        if execution != TaskExecution::Direct {
            return HashSet::new();
        }

        let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
        let mut skipped: HashSet<String> = setup_tasks
            .iter()
            .filter(|task| task.contributor.as_deref() == Some("cuenv"))
            .map(|task| task.id.clone())
            .collect();

        let mut changed = true;
        while changed {
            changed = false;

            for task in &setup_tasks {
                if skipped.contains(&task.id) {
                    continue;
                }

                if task.depends_on.iter().any(|dep| skipped.contains(dep)) {
                    skipped.insert(task.id.clone());
                    changed = true;
                }
            }
        }

        skipped
    }

    fn cuenv_setup_task_ids(ir: &IntermediateRepresentation) -> HashSet<String> {
        ir.sorted_phase_tasks(BuildStage::Setup)
            .iter()
            .filter(|task| task.contributor.as_deref() == Some("cuenv"))
            .map(|task| task.id.clone())
            .collect()
    }

    fn skipped_setup_task_ids(
        ir: &IntermediateRepresentation,
        execution: TaskExecution,
        cuenv_setup: &CuenvSetup,
    ) -> HashSet<String> {
        if execution == TaskExecution::Direct {
            return Self::direct_execution_skipped_setup_task_ids(ir, execution);
        }

        match cuenv_setup {
            CuenvSetup::BuildInJob => HashSet::new(),
            CuenvSetup::DownloadArtifact { .. } => Self::cuenv_setup_task_ids(ir),
        }
    }

    fn cuenv_artifact_steps(artifact_name: &str) -> Vec<Step> {
        vec![
            Step::uses("actions/download-artifact@v4")
                .with_name("Download cuenv")
                .with_input("name", serde_yaml::Value::String(artifact_name.to_string()))
                .with_input(
                    "path",
                    serde_yaml::Value::String(CUENV_BOOTSTRAP_ARTIFACT_DIR.to_string()),
                ),
            Step::run(format!(
                "chmod +x \"{CUENV_BOOTSTRAP_ARTIFACT_PATH}/cuenv\"\necho \"{CUENV_BOOTSTRAP_ARTIFACT_PATH}\" >> \"$GITHUB_PATH\""
            ))
            .with_name("Add cuenv to PATH"),
        ]
    }

    fn should_include_in_cuenv_bootstrap(
        task: &Task,
        skipped_setup_task_ids: &HashSet<String>,
        cuenv_setup_task_ids: &HashSet<String>,
    ) -> bool {
        !skipped_setup_task_ids.contains(&task.id) || cuenv_setup_task_ids.contains(&task.id)
    }

    /// Build a dedicated job that builds cuenv once and uploads it for reuse.
    ///
    /// The job renders:
    /// 1. Checkout
    /// 2. Bootstrap phase tasks (Nix install, etc.)
    /// 3. Setup tasks required before cuenv
    /// 4. The cuenv setup task itself
    ///
    /// Setup tasks that depend on cuenv (for example 1Password setup) are
    /// intentionally excluded. They run in downstream jobs after those jobs
    /// download the uploaded cuenv artifact.
    #[must_use]
    pub fn build_cuenv_bootstrap_job(
        &self,
        ir: &IntermediateRepresentation,
        options: CuenvBootstrapJobOptions<'_>,
    ) -> Option<Job> {
        if !self.build_cuenv {
            return None;
        }

        let cuenv_setup_task_ids = Self::cuenv_setup_task_ids(ir);
        if cuenv_setup_task_ids.is_empty() {
            return None;
        }

        let renderer = self.stage_renderer();
        let skipped_setup_task_ids =
            Self::direct_execution_skipped_setup_task_ids(ir, TaskExecution::Direct);
        let mut steps = Vec::new();

        steps.push(
            Step::uses("actions/checkout@v4")
                .with_name("Checkout")
                .with_input("fetch-depth", serde_yaml::Value::Number(2.into())),
        );

        let bootstrap_steps = renderer.render_tasks(&ir.sorted_phase_tasks(BuildStage::Bootstrap));
        steps.extend(bootstrap_steps);

        for task in ir.sorted_phase_tasks(BuildStage::Setup) {
            if !Self::should_include_in_cuenv_bootstrap(
                task,
                &skipped_setup_task_ids,
                &cuenv_setup_task_ids,
            ) {
                continue;
            }

            steps.push(renderer.render_task(task));
        }

        let mut upload_step = Step::uses("actions/upload-artifact@v4")
            .with_name("Upload cuenv")
            .with_input(
                "name",
                serde_yaml::Value::String(options.artifact_name.to_string()),
            )
            .with_input(
                "path",
                serde_yaml::Value::String("result/bin/cuenv".to_string()),
            );
        upload_step.with_inputs.insert(
            "if-no-files-found".to_string(),
            serde_yaml::Value::String("error".to_string()),
        );
        steps.push(upload_step);

        Some(Job {
            name: Some(options.name.to_string()),
            runs_on: options.runs_on,
            needs: Vec::new(),
            if_condition: None,
            strategy: None,
            environment: None,
            env: IndexMap::new(),
            concurrency: None,
            continue_on_error: None,
            timeout_minutes: Some(30),
            steps,
        })
    }

    /// Render phase tasks (bootstrap + setup) into GitHub Actions steps.
    ///
    /// Returns a tuple of:
    /// - `Vec<Step>` - rendered steps for bootstrap and setup phase tasks
    /// - `IndexMap<String, String>` - secret env vars that should be passed to task steps
    ///
    /// This uses `GitHubStageRenderer` to properly convert phase tasks into steps,
    /// handling both `uses:` action steps and `run:` command steps.
    #[must_use]
    pub fn render_phase_steps(
        &self,
        ir: &IntermediateRepresentation,
        execution: TaskExecution,
        cuenv_setup: &CuenvSetup,
    ) -> (Vec<Step>, IndexMap<String, String>) {
        let renderer = self.stage_renderer();
        let mut steps = Vec::new();
        let mut secret_env_vars = IndexMap::new();
        let skipped_setup_task_ids = Self::skipped_setup_task_ids(ir, execution, cuenv_setup);

        let bootstrap_steps = renderer.render_tasks(&ir.sorted_phase_tasks(BuildStage::Bootstrap));
        steps.extend(bootstrap_steps);

        if let CuenvSetup::DownloadArtifact { artifact_name } = cuenv_setup {
            steps.extend(Self::cuenv_artifact_steps(artifact_name));
        }

        for task in ir.sorted_phase_tasks(BuildStage::Setup) {
            if skipped_setup_task_ids.contains(&task.id) {
                continue;
            }
            let step = renderer.render_task(task);
            steps.push(step);

            for (key, value) in &task.env {
                secret_env_vars.insert(key.clone(), value.clone());
            }
        }

        (steps, secret_env_vars)
    }

    /// Build a simple job from an IR task (no matrix expansion).
    ///
    /// This method creates a single job that:
    /// 1. Checks out the repository
    /// 2. Runs bootstrap/setup phase tasks (Nix, cuenv, 1Password, etc.)
    /// 3. Runs the task either directly or with `--skip-dependencies`
    ///
    /// Use `build_matrix_jobs` for tasks with matrix configurations.
    ///
    /// # Arguments
    ///
    /// * `task` - IR task to build job for
    /// * `ir` - Intermediate representation containing phase tasks
    /// * `options` - Execution mode, optional environment, and working directory
    #[must_use]
    pub fn build_simple_job(
        &self,
        task: &Task,
        ir: &IntermediateRepresentation,
        options: &SimpleJobOptions<'_>,
    ) -> Job {
        let mut steps = Vec::new();

        steps.push(
            Step::uses("actions/checkout@v4")
                .with_name("Checkout")
                .with_input("fetch-depth", serde_yaml::Value::Number(2.into())),
        );

        let (phase_steps, secret_env_vars) =
            self.render_phase_steps(ir, options.execution, &options.cuenv_setup);
        steps.extend(phase_steps);

        for artifact in &task.artifact_downloads {
            let download_step = Step::uses("actions/download-artifact@v4")
                .with_name(format!("Download {}", artifact.name))
                .with_input("name", serde_yaml::Value::String(artifact.name.clone()))
                .with_input("path", serde_yaml::Value::String(artifact.path.clone()));
            steps.push(download_step);
        }

        let mut task_step = match options.execution {
            TaskExecution::Orchestrated => {
                let task_command = options.environment.map_or_else(
                    || format!("cuenv task {} --skip-dependencies", task.id),
                    |env| format!("cuenv task {} -e {} --skip-dependencies", task.id, env),
                );
                let mut step = Step::run(task_command).with_name(task.label());
                Self::add_github_context_env(&mut step);

                for (key, value) in &task.env {
                    step.env.insert(key.clone(), transform_secret_ref(value));
                }

                step
            }
            TaskExecution::Direct => {
                let mut step = self.stage_renderer().render_task(task);
                Self::add_github_context_env(&mut step);
                step
            }
        };

        if task_step.run.is_some()
            && let Some(path) = options.project_path
        {
            task_step = task_step.with_working_directory(path);
        }

        for (key, value) in secret_env_vars {
            task_step.env.insert(key, transform_secret_ref(&value));
        }

        steps.push(task_step);

        let orchestrator_outputs: Vec<_> = task
            .outputs
            .iter()
            .filter(|o| o.output_type == OutputType::Orchestrator)
            .collect();

        if !orchestrator_outputs.is_empty() {
            let paths: Vec<String> = orchestrator_outputs
                .iter()
                .map(|o| o.path.clone())
                .collect();
            let mut upload_step = Step::uses("actions/upload-artifact@v4")
                .with_name("Upload artifacts")
                .with_input(
                    "name",
                    serde_yaml::Value::String(format!("{}-artifacts", task.id.replace('.', "-"))),
                )
                .with_input("path", serde_yaml::Value::String(paths.join("\n")));
            upload_step.with_inputs.insert(
                "if-no-files-found".to_string(),
                serde_yaml::Value::String("ignore".to_string()),
            );
            upload_step.with_inputs.insert(
                "include-hidden-files".to_string(),
                serde_yaml::Value::Bool(true),
            );
            steps.push(upload_step);
        }

        Job {
            name: Some(task.id.clone()),
            runs_on: RunsOn::Label(self.runner.clone()),
            needs: Vec::new(),
            if_condition: None,
            strategy: None,
            environment: None,
            env: IndexMap::new(),
            concurrency: None,
            continue_on_error: None,
            timeout_minutes: None,
            steps,
        }
    }

    /// Build an artifact aggregation job from an IR task with `artifact_downloads`.
    ///
    /// This creates a job that:
    /// 1. Checks out the repository
    /// 2. Runs bootstrap/setup phase tasks
    /// 3. Downloads artifacts from previous jobs
    /// 4. Runs the task with params and `--skip-dependencies`
    ///
    /// Use this for tasks that aggregate outputs from matrix jobs (e.g., publish).
    ///
    /// # Arguments
    ///
    /// * `task` - IR task to build job for
    /// * `ir` - Intermediate representation containing phase tasks
    /// * `options` - Environment, dependencies, working directory, and cuenv setup mode
    #[must_use]
    pub fn build_artifact_aggregation_job(
        &self,
        task: &Task,
        ir: &IntermediateRepresentation,
        options: &ArtifactAggregationJobOptions<'_>,
    ) -> Job {
        let mut steps = Vec::new();

        steps.push(
            Step::uses("actions/checkout@v4")
                .with_name("Checkout")
                .with_input("fetch-depth", serde_yaml::Value::Number(0.into())),
        );

        let (phase_steps, secret_env_vars) =
            self.render_phase_steps(ir, TaskExecution::Orchestrated, &options.cuenv_setup);
        steps.extend(phase_steps);

        for artifact in &task.artifact_downloads {
            for prev_job in options.previous_jobs {
                let source_prefix = artifact.name.replace('.', "-");
                if prev_job.starts_with(&source_prefix) || prev_job.contains(&artifact.name) {
                    let suffix = prev_job
                        .strip_prefix(&source_prefix)
                        .unwrap_or("")
                        .trim_start_matches('-');

                    let download_path = if suffix.is_empty() {
                        artifact.path.clone()
                    } else {
                        format!("{}/{}", artifact.path, suffix)
                    };

                    steps.push(
                        Step::uses("actions/download-artifact@v4")
                            .with_name(format!("Download {prev_job}"))
                            .with_input("name", serde_yaml::Value::String(prev_job.clone()))
                            .with_input("path", serde_yaml::Value::String(download_path)),
                    );
                }
            }
        }

        let task_command = options.environment.map_or_else(
            || format!("cuenv task {} --skip-dependencies", task.id),
            |env| format!("cuenv task {} -e {} --skip-dependencies", task.id, env),
        );

        let mut task_step = Step::run(&task_command).with_name(task.id.clone());
        Self::add_github_context_env(&mut task_step);

        if let Some(path) = options.project_path {
            task_step = task_step.with_working_directory(path);
        }

        for (key, value) in &task.params {
            task_step.env.insert(key.to_uppercase(), value.clone());
        }

        for (key, value) in &task.env {
            task_step
                .env
                .insert(key.clone(), transform_secret_ref(value));
        }

        for (key, value) in secret_env_vars {
            task_step.env.insert(key, transform_secret_ref(&value));
        }

        steps.push(task_step);

        Job {
            name: Some(task.id.clone()),
            runs_on: RunsOn::Label(self.runner.clone()),
            needs: options.previous_jobs.to_vec(),
            if_condition: None,
            strategy: None,
            environment: None,
            env: IndexMap::new(),
            concurrency: None,
            continue_on_error: None,
            timeout_minutes: Some(30),
            steps,
        }
    }

    /// Build matrix-expanded jobs from an IR task with `matrix` configuration.
    ///
    /// This expands a single task into multiple jobs, one per matrix combination.
    /// Currently supports single-dimension matrix expansion (arch).
    ///
    /// Returns an `IndexMap` of `job_id` -> Job for each matrix combination.
    ///
    /// # Arguments
    ///
    /// * `task` - IR task with `matrix` configuration
    /// * `ir` - Intermediate representation containing phase tasks
    /// * `options` - Environment, runner, dependency, working directory, and cuenv setup options
    #[must_use]
    pub fn build_matrix_jobs(
        &self,
        task: &Task,
        ir: &IntermediateRepresentation,
        options: &MatrixJobOptions<'_>,
    ) -> IndexMap<String, Job> {
        let mut jobs = IndexMap::new();
        let base_job_id = task.id.replace(['.', ' '], "-");

        let Some(matrix) = &task.matrix else {
            return jobs;
        };

        if let Some(arch_values) = matrix.dimensions.get("arch") {
            for arch in arch_values {
                let job_id = format!("{base_job_id}-{arch}");

                let runner = options
                    .arch_runners
                    .and_then(|m| m.get(arch))
                    .cloned()
                    .unwrap_or_else(|| self.runner.clone());

                let mut steps = Vec::new();

                steps.push(
                    Step::uses("actions/checkout@v4")
                        .with_name("Checkout")
                        .with_input("fetch-depth", serde_yaml::Value::Number(0.into())),
                );

                let cuenv_setup = options
                    .cuenv_artifacts_by_runner
                    .and_then(|artifacts| artifacts.get(&runner))
                    .map_or(CuenvSetup::BuildInJob, |artifact_name| {
                        CuenvSetup::DownloadArtifact {
                            artifact_name: artifact_name.clone(),
                        }
                    });
                let (phase_steps, secret_env_vars) =
                    self.render_phase_steps(ir, TaskExecution::Orchestrated, &cuenv_setup);
                steps.extend(phase_steps);

                let task_command = options.environment.map_or_else(
                    || format!("cuenv task {} --skip-dependencies", task.id),
                    |env| format!("cuenv task {} -e {} --skip-dependencies", task.id, env),
                );
                let mut task_step =
                    Step::run(&task_command).with_name(format!("{} ({arch})", task.id));
                Self::add_github_context_env(&mut task_step);

                if let Some(path) = options.project_path {
                    task_step = task_step.with_working_directory(path);
                }

                task_step.env.insert("CUENV_ARCH".to_string(), arch.clone());

                for (key, value) in &task.env {
                    task_step
                        .env
                        .insert(key.clone(), transform_secret_ref(value));
                }

                for (key, value) in &secret_env_vars {
                    task_step
                        .env
                        .insert(key.clone(), transform_secret_ref(value));
                }

                steps.push(task_step);

                let artifact_path = if task.outputs.is_empty() {
                    "result/bin/*".to_string()
                } else {
                    task.outputs
                        .iter()
                        .map(|o| o.path.clone())
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                let mut upload_step = Step::uses("actions/upload-artifact@v4")
                    .with_name("Upload artifacts")
                    .with_input(
                        "name",
                        serde_yaml::Value::String(format!("{base_job_id}-{arch}")),
                    )
                    .with_input("path", serde_yaml::Value::String(artifact_path));
                upload_step.with_inputs.insert(
                    "if-no-files-found".to_string(),
                    serde_yaml::Value::String("ignore".to_string()),
                );
                upload_step.with_inputs.insert(
                    "include-hidden-files".to_string(),
                    serde_yaml::Value::Bool(true),
                );
                steps.push(upload_step);

                jobs.insert(
                    job_id,
                    Job {
                        name: Some(format!("{} ({arch})", task.id)),
                        runs_on: RunsOn::Label(runner),
                        needs: options.previous_jobs.to_vec(),
                        if_condition: None,
                        strategy: None,
                        environment: None,
                        env: IndexMap::new(),
                        concurrency: None,
                        continue_on_error: None,
                        timeout_minutes: Some(60),
                        steps,
                    },
                );
            }
        }

        jobs
    }

    /// Check if a task has matrix configuration.
    #[must_use]
    pub fn task_has_matrix(task: &Task) -> bool {
        task.matrix
            .as_ref()
            .is_some_and(|m| !m.dimensions.is_empty())
    }

    /// Check if a task has artifact downloads (aggregation task).
    #[must_use]
    pub const fn task_has_artifact_downloads(task: &Task) -> bool {
        !task.artifact_downloads.is_empty()
    }
}
