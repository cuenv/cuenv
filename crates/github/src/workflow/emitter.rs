//! GitHub Actions Workflow Emitter
//!
//! Transforms cuenv IR into GitHub Actions workflow YAML files.

use crate::workflow::schema::{
    Concurrency, Environment, Job, Matrix, PermissionLevel, Permissions, PullRequestTrigger,
    PushTrigger, ReleaseTrigger, RunsOn, ScheduleTrigger, Step, Strategy, Workflow,
    WorkflowDispatchTrigger, WorkflowInput, WorkflowTriggers,
};
use crate::workflow::stage_renderer::{transform_secret_ref, GitHubStageRenderer};
use cuenv_ci::emitter::{Emitter, EmitterError, EmitterResult};
use cuenv_ci::ir::{IntermediateRepresentation, OutputType, StageConfiguration, Task, TriggerCondition};
use cuenv_ci::StageRenderer;
use indexmap::IndexMap;
use std::collections::HashMap;

/// GitHub Actions workflow emitter
///
/// Transforms cuenv IR into GitHub Actions workflow YAML that can be
/// committed to `.github/workflows/`.
///
/// # IR to GitHub Actions Mapping
///
/// | IR Field | GitHub Actions |
/// |----------|----------------|
/// | `pipeline.name` | Workflow `name:` |
/// | `pipeline.trigger.branch` | `on.push.branches` / `on.pull_request.branches` |
/// | `task.id` | Job key |
/// | `task.command` | Step with `run: cuenv task {task.id}` |
/// | `task.depends_on` | Job `needs:` |
/// | `task.manual_approval` | Job with `environment:` |
/// | `task.concurrency_group` | Job-level `concurrency:` |
/// | `task.resources.tags` | `runs-on:` |
/// | `task.outputs` (orchestrator) | `actions/upload-artifact` step |
#[derive(Debug, Clone)]
pub struct GitHubActionsEmitter {
    /// Default runner for jobs
    pub runner: String,
    /// Include Nix installation steps
    pub use_nix: bool,
    /// Include Cachix caching steps
    pub use_cachix: bool,
    /// Cachix cache name
    pub cachix_name: Option<String>,
    /// Cachix auth token secret name
    pub cachix_auth_token_secret: String,
    /// Default paths to ignore in triggers
    pub default_paths_ignore: Vec<String>,
    /// Include cuenv build step (via nix build)
    pub build_cuenv: bool,
    /// Environment name for manual approval tasks
    pub approval_environment: String,
}

impl Default for GitHubActionsEmitter {
    fn default() -> Self {
        Self {
            runner: "ubuntu-latest".to_string(),
            use_nix: true,
            use_cachix: false,
            cachix_name: None,
            cachix_auth_token_secret: "CACHIX_AUTH_TOKEN".to_string(),
            default_paths_ignore: vec![
                "docs/**".to_string(),
                "examples/**".to_string(),
                "*.md".to_string(),
                "LICENSE".to_string(),
                ".vscode/**".to_string(),
            ],
            build_cuenv: true,
            approval_environment: "production".to_string(),
        }
    }
}

impl GitHubActionsEmitter {
    /// Create a new GitHub Actions emitter with default settings
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an emitter from a `GitHubConfig` manifest configuration.
    ///
    /// This applies all configuration from the CUE manifest to the emitter.
    #[must_use]
    pub fn from_config(config: &crate::config::GitHubConfig) -> Self {
        let mut emitter = Self::default();

        // Apply runner configuration
        if let Some(runner) = &config.runner {
            emitter.runner = runner.as_single().unwrap_or("ubuntu-latest").to_string();
        }

        // Apply Cachix configuration
        if let Some(cachix) = &config.cachix {
            emitter.use_cachix = true;
            emitter.cachix_name = Some(cachix.name.clone());
            if let Some(auth_token) = &cachix.auth_token {
                emitter.cachix_auth_token_secret.clone_from(auth_token);
            }
        }

        // Apply paths ignore
        if let Some(paths_ignore) = &config.paths_ignore {
            emitter.default_paths_ignore.clone_from(paths_ignore);
        }

        emitter
    }

    /// Get the configured runner as a `RunsOn` value
    #[must_use]
    pub fn runner_as_runs_on(&self) -> RunsOn {
        RunsOn::Label(self.runner.clone())
    }

    /// Set the default runner for jobs
    #[must_use]
    pub fn with_runner(mut self, runner: impl Into<String>) -> Self {
        self.runner = runner.into();
        self
    }

    /// Enable Nix installation steps
    #[must_use]
    pub const fn with_nix(mut self) -> Self {
        self.use_nix = true;
        self
    }

    /// Disable Nix installation steps
    #[must_use]
    pub const fn without_nix(mut self) -> Self {
        self.use_nix = false;
        self
    }

    /// Enable Cachix caching with the given cache name
    #[must_use]
    pub fn with_cachix(mut self, name: impl Into<String>) -> Self {
        self.use_cachix = true;
        self.cachix_name = Some(name.into());
        self
    }

    /// Set the Cachix auth token secret name
    #[must_use]
    pub fn with_cachix_auth_token_secret(mut self, secret: impl Into<String>) -> Self {
        self.cachix_auth_token_secret = secret.into();
        self
    }

    /// Set default paths to ignore in triggers
    #[must_use]
    pub fn with_paths_ignore(mut self, paths: Vec<String>) -> Self {
        self.default_paths_ignore = paths;
        self
    }

    /// Disable automatic cuenv build step
    #[must_use]
    pub const fn without_cuenv_build(mut self) -> Self {
        self.build_cuenv = false;
        self
    }

    /// Set the environment name for manual approval tasks
    #[must_use]
    pub fn with_approval_environment(mut self, env: impl Into<String>) -> Self {
        self.approval_environment = env.into();
        self
    }

    /// Emit multiple workflow files for projects with multiple pipelines.
    ///
    /// Returns a map of filename to YAML content.
    /// Each pipeline in the IR generates a separate workflow file.
    ///
    /// # Errors
    ///
    /// Returns `EmitterError::Serialization` if YAML serialization fails.
    pub fn emit_workflows(
        &self,
        ir: &IntermediateRepresentation,
    ) -> EmitterResult<HashMap<String, String>> {
        let mut workflows = HashMap::new();

        // Build workflow name with optional project prefix for monorepo support
        let workflow_name = Self::build_workflow_name(ir);

        // Generate a single workflow with all tasks as jobs
        let workflow = self.build_workflow(ir, &workflow_name);
        let filename = format!("{}.yml", sanitize_filename(&workflow_name));
        let yaml = Self::serialize_workflow(&workflow)?;
        workflows.insert(filename, yaml);

        Ok(workflows)
    }

    /// Build the workflow name, prefixing with project name if available (for monorepo support)
    fn build_workflow_name(ir: &IntermediateRepresentation) -> String {
        ir.pipeline.project_name.as_ref().map_or_else(
            || ir.pipeline.name.clone(),
            |project| format!("{}-{}", project, ir.pipeline.name),
        )
    }

    /// Build a workflow from the IR
    fn build_workflow(&self, ir: &IntermediateRepresentation, workflow_name: &str) -> Workflow {
        let triggers = self.build_triggers(ir);
        let permissions = Self::build_permissions(ir);
        let jobs = self.build_jobs(ir);

        Workflow {
            name: workflow_name.to_string(),
            on: triggers,
            concurrency: Some(Concurrency {
                group: "${{ github.workflow }}-${{ github.head_ref || github.ref }}".to_string(),
                cancel_in_progress: Some(true),
            }),
            permissions: Some(permissions),
            env: IndexMap::new(),
            jobs,
        }
    }

    /// Build workflow triggers from IR
    fn build_triggers(&self, ir: &IntermediateRepresentation) -> WorkflowTriggers {
        let trigger = ir.pipeline.trigger.as_ref();

        WorkflowTriggers {
            push: self.build_push_trigger(trigger),
            pull_request: self.build_pr_trigger(trigger),
            release: Self::build_release_trigger(trigger),
            workflow_dispatch: Self::build_manual_trigger(trigger),
            schedule: Self::build_schedule_trigger(trigger),
        }
    }

    /// Build push trigger from IR trigger condition
    fn build_push_trigger(&self, trigger: Option<&TriggerCondition>) -> Option<PushTrigger> {
        let trigger = trigger?;

        // Only emit push trigger if we have branch conditions
        if trigger.branches.is_empty() {
            return None;
        }

        Some(PushTrigger {
            branches: trigger.branches.clone(),
            paths: trigger.paths.clone(),
            paths_ignore: if trigger.paths_ignore.is_empty() {
                self.default_paths_ignore.clone()
            } else {
                trigger.paths_ignore.clone()
            },
            ..Default::default()
        })
    }

    /// Build pull request trigger from IR trigger condition
    fn build_pr_trigger(&self, trigger: Option<&TriggerCondition>) -> Option<PullRequestTrigger> {
        let trigger = trigger?;

        // Only emit PR trigger if explicitly enabled - never default to running on PRs
        if trigger.pull_request == Some(true) {
            Some(PullRequestTrigger {
                branches: trigger.branches.clone(),
                paths: trigger.paths.clone(),
                paths_ignore: if trigger.paths_ignore.is_empty() {
                    self.default_paths_ignore.clone()
                } else {
                    trigger.paths_ignore.clone()
                },
                ..Default::default()
            })
        } else {
            None
        }
    }

    /// Build release trigger from IR trigger condition
    fn build_release_trigger(trigger: Option<&TriggerCondition>) -> Option<ReleaseTrigger> {
        let trigger = trigger?;

        if trigger.release.is_empty() {
            return None;
        }

        Some(ReleaseTrigger {
            types: trigger.release.clone(),
        })
    }

    /// Build schedule trigger from IR trigger condition
    fn build_schedule_trigger(trigger: Option<&TriggerCondition>) -> Option<Vec<ScheduleTrigger>> {
        let trigger = trigger?;

        if trigger.scheduled.is_empty() {
            return None;
        }

        Some(
            trigger
                .scheduled
                .iter()
                .map(|cron| ScheduleTrigger { cron: cron.clone() })
                .collect(),
        )
    }

    /// Build manual (`workflow_dispatch`) trigger from IR trigger condition
    fn build_manual_trigger(trigger: Option<&TriggerCondition>) -> Option<WorkflowDispatchTrigger> {
        let trigger = trigger?;
        let manual = trigger.manual.as_ref()?;

        if !manual.enabled && manual.inputs.is_empty() {
            return None;
        }

        Some(WorkflowDispatchTrigger {
            inputs: manual
                .inputs
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        WorkflowInput {
                            description: v.description.clone(),
                            required: Some(v.required),
                            default: v.default.clone(),
                            input_type: v.input_type.clone(),
                            options: if v.options.is_empty() {
                                None
                            } else {
                                Some(v.options.clone())
                            },
                        },
                    )
                })
                .collect(),
        })
    }

    /// Build permissions based on task requirements
    fn build_permissions(ir: &IntermediateRepresentation) -> Permissions {
        let has_deployments = ir.tasks.iter().any(|t| t.deployment);
        let has_outputs = ir.tasks.iter().any(|t| {
            t.outputs
                .iter()
                .any(|o| o.output_type == OutputType::Orchestrator)
        });

        Permissions {
            contents: Some(if has_deployments {
                PermissionLevel::Write
            } else {
                PermissionLevel::Read
            }),
            checks: Some(PermissionLevel::Write),
            pull_requests: Some(PermissionLevel::Write),
            packages: if has_outputs {
                Some(PermissionLevel::Write)
            } else {
                None
            },
            ..Default::default()
        }
    }

    /// Build jobs from IR tasks
    fn build_jobs(&self, ir: &IntermediateRepresentation) -> IndexMap<String, Job> {
        let mut jobs = IndexMap::new();

        for task in &ir.tasks {
            let job = self.build_job(task, ir);
            jobs.insert(sanitize_job_id(&task.id), job);
        }

        jobs
    }

    /// Build a job from an IR task
    #[allow(clippy::too_many_lines)]
    fn build_job(&self, task: &Task, ir: &IntermediateRepresentation) -> Job {
        let mut steps = Vec::new();

        // Checkout step
        steps.push(
            Step::uses("actions/checkout@v4")
                .with_name("Checkout")
                .with_input("fetch-depth", serde_yaml::Value::Number(2.into())),
        );

        // Check IR stages for what setup is needed
        let has_install_nix = ir.stages.bootstrap.iter().any(|t| t.id == "install-nix");
        let has_cachix = ir.stages.setup.iter().any(|t| t.id == "setup-cachix");
        let has_1password = ir.stages.setup.iter().any(|t| t.id == "setup-1password");

        // Nix installation (from install-nix stage task)
        if has_install_nix {
            steps.push(
                Step::uses("DeterminateSystems/nix-installer-action@v16")
                    .with_name("Install Nix")
                    .with_input(
                        "extra-conf",
                        serde_yaml::Value::String("accept-flake-config = true".to_string()),
                    ),
            );
        }

        // Cachix setup (from setup-cachix stage task)
        if has_cachix {
            // Get cachix config from the stage task
            if let Some(cachix_task) = ir.stages.setup.iter().find(|t| t.id == "setup-cachix") {
                // Extract cache name from the task command
                // Command format: ". /nix/... && nix-env -iA cachix... && cachix use {name}"
                let cache_name = cachix_task
                    .command
                    .first()
                    .and_then(|cmd| cmd.split("cachix use ").nth(1))
                    .unwrap_or("cuenv");

                // Get auth token secret name from env
                let auth_token_secret = cachix_task
                    .env
                    .get("CACHIX_AUTH_TOKEN")
                    .and_then(|v| {
                        // Extract secret name from ${SECRET_NAME}
                        v.strip_prefix("${").and_then(|s| s.strip_suffix("}"))
                    })
                    .unwrap_or("CACHIX_AUTH_TOKEN");

                let mut cachix_step = Step::uses("cachix/cachix-action@v15")
                    .with_name("Setup Cachix")
                    .with_input("name", serde_yaml::Value::String(cache_name.to_string()))
                    .with_input(
                        "authToken",
                        serde_yaml::Value::String(format!(
                            "${{{{ secrets.{auth_token_secret} }}}}"
                        )),
                    );
                cachix_step.with_inputs.insert(
                    "pushFilter".to_string(),
                    serde_yaml::Value::String("(-source$|nixpkgs\\.tar\\.gz$)".to_string()),
                );
                steps.push(cachix_step);
            }
        }

        // Setup cuenv (from setup-cuenv stage task)
        if let Some(cuenv_task) = ir.stages.setup.iter().find(|t| t.id == "setup-cuenv") {
            let command = cuenv_task.command.first().cloned().unwrap_or_default();
            let name = cuenv_task
                .label
                .clone()
                .unwrap_or_else(|| "Setup cuenv".to_string());
            steps.push(Step::run(&command).with_name(&name));
        }

        // Setup 1Password (from setup-1password stage task)
        if has_1password {
            steps.push(Step::run("cuenv secrets setup onepassword").with_name("Setup 1Password"));
        }

        // Setup GitHub Models CLI extension (from setup-gh-models stage task)
        if let Some(gh_models_task) = ir.stages.setup.iter().find(|t| t.id == "setup-gh-models") {
            let command = gh_models_task.command.first().cloned().unwrap_or_default();
            let name = gh_models_task
                .label
                .clone()
                .unwrap_or_else(|| "Setup GitHub Models CLI".to_string());
            steps.push(
                Step::run(&command)
                    .with_name(&name)
                    .with_env("GH_TOKEN", "${{ github.token }}"),
            );
        }

        // Run the task
        // Use --skip-dependencies (-S) because GitHub Actions handles job dependencies
        // via the `needs:` field. Each job only needs to run its own task.
        let environment = ir.pipeline.environment.as_deref();
        let task_command = environment.map_or_else(
            || format!("cuenv task {} --skip-dependencies", task.id),
            |env| format!("cuenv task {} -e {} --skip-dependencies", task.id, env),
        );
        let mut task_step = Step::run(task_command)
            .with_name(task.id.clone())
            .with_env("GITHUB_TOKEN", "${{ secrets.GITHUB_TOKEN }}");

        // Add task environment variables (transform secret references)
        for (key, value) in &task.env {
            task_step
                .env
                .insert(key.clone(), transform_secret_ref(value));
        }

        // Add 1Password service account token if needed
        if has_1password {
            task_step.env.insert(
                "OP_SERVICE_ACCOUNT_TOKEN".to_string(),
                "${{ secrets.OP_SERVICE_ACCOUNT_TOKEN }}".to_string(),
            );
        }

        steps.push(task_step);

        // Upload artifacts for orchestrator outputs
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
            let mut artifact_step = Step::uses("actions/upload-artifact@v4")
                .with_name("Upload artifacts")
                .with_input(
                    "name",
                    serde_yaml::Value::String(format!("{}-artifacts", task.id)),
                )
                .with_input("path", serde_yaml::Value::String(paths.join("\n")));
            artifact_step.with_inputs.insert(
                "if-no-files-found".to_string(),
                serde_yaml::Value::String("ignore".to_string()),
            );
            artifact_step.if_condition = Some("always()".to_string());
            steps.push(artifact_step);
        }

        // Determine runner
        let runs_on = task
            .resources
            .as_ref()
            .and_then(|r| r.tags.first())
            .map_or_else(
                || RunsOn::Label(self.runner.clone()),
                |tag| RunsOn::Label(tag.clone()),
            );

        // Map dependencies to sanitized job IDs
        let needs: Vec<String> = task.depends_on.iter().map(|d| sanitize_job_id(d)).collect();

        // Handle manual approval via environment
        let environment = if task.manual_approval {
            Some(Environment::Name(self.approval_environment.clone()))
        } else {
            None
        };

        // Handle concurrency group
        let concurrency = task.concurrency_group.as_ref().map(|group| Concurrency {
            group: group.clone(),
            cancel_in_progress: Some(false),
        });

        Job {
            name: Some(task.id.clone()),
            runs_on,
            needs,
            if_condition: None,
            strategy: None,
            environment,
            env: IndexMap::new(),
            concurrency,
            continue_on_error: None,
            timeout_minutes: None,
            steps,
        }
    }

    /// Serialize a workflow to YAML with a generation header
    fn serialize_workflow(workflow: &Workflow) -> EmitterResult<String> {
        let yaml = serde_yaml::to_string(workflow)
            .map_err(|e| EmitterError::Serialization(e.to_string()))?;

        // Add generation header
        let header = "# Generated by cuenv - do not edit manually\n# Regenerate with: cuenv ci --format github\n\n";

        Ok(format!("{header}{yaml}"))
    }

    // =========================================================================
    // Matrix and Artifact Job Building Methods
    // =========================================================================

    /// Render stage configuration (bootstrap + setup) into GitHub Actions steps.
    ///
    /// Returns a tuple of:
    /// - `Vec<Step>` - rendered steps for bootstrap and setup stages
    /// - `IndexMap<String, String>` - secret env vars that should be passed to task steps
    ///
    /// This uses `GitHubStageRenderer` to properly convert stage tasks into steps,
    /// handling both `uses:` action steps and `run:` command steps.
    #[must_use]
    pub fn render_stage_steps(
        stages: &StageConfiguration,
    ) -> (Vec<Step>, IndexMap<String, String>) {
        let renderer = GitHubStageRenderer::new();
        let mut steps = Vec::new();
        let mut secret_env_vars = IndexMap::new();

        // Render bootstrap stage tasks (e.g., Nix installation)
        // Note: render_bootstrap returns Result<_, Infallible> so unwrap is safe
        let bootstrap_steps = renderer.render_bootstrap(stages).unwrap();
        steps.extend(bootstrap_steps);

        // Render setup stage tasks (e.g., cuenv, 1Password, Cachix)
        // Also collect env vars from setup tasks that need to be passed to task steps
        for task in &stages.setup {
            // Note: render_task returns Result<_, Infallible> so unwrap is safe
            let step = renderer.render_task(task).unwrap();
            steps.push(step);

            // Collect env vars from setup tasks - these may contain secrets
            // that need to be available when the actual task runs
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
    /// 2. Runs bootstrap/setup stages (Nix, cuenv, 1Password, etc.)
    /// 3. Runs the task with `--skip-dependencies` (since CI handles job dependencies)
    ///
    /// Use `build_matrix_jobs` for tasks with matrix configurations.
    #[must_use]
    pub fn build_simple_job(
        &self,
        task: &Task,
        stages: &StageConfiguration,
        environment: Option<&String>,
    ) -> Job {
        let mut steps = Vec::new();

        // Checkout
        steps.push(
            Step::uses("actions/checkout@v4")
                .with_name("Checkout")
                .with_input("fetch-depth", serde_yaml::Value::Number(2.into())),
        );

        // Render bootstrap and setup stages using the StageRenderer
        let (stage_steps, secret_env_vars) = Self::render_stage_steps(stages);
        steps.extend(stage_steps);

        // Run the task
        // Use --skip-dependencies because GitHub Actions handles job dependencies via `needs:`
        let task_command = environment.map_or_else(
            || format!("cuenv task {} --skip-dependencies", task.id),
            |env| format!("cuenv task {} -e {} --skip-dependencies", task.id, env),
        );
        let mut task_step = Step::run(task_command)
            .with_name(task.id.clone())
            .with_env("GITHUB_TOKEN", "${{ secrets.GITHUB_TOKEN }}");

        // Add secret env vars from setup stages to the task step
        for (key, value) in secret_env_vars {
            task_step.env.insert(key, transform_secret_ref(&value));
        }

        // Add task-level env vars
        for (key, value) in &task.env {
            task_step.env.insert(key.clone(), transform_secret_ref(value));
        }

        steps.push(task_step);

        Job {
            name: Some(task.id.clone()),
            runs_on: RunsOn::Label(self.runner.clone()),
            needs: Vec::new(), // Caller should set this based on depends_on
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
    /// 2. Runs bootstrap/setup stages
    /// 3. Downloads artifacts from previous jobs
    /// 4. Runs the task with params and `--skip-dependencies`
    ///
    /// Use this for tasks that aggregate outputs from matrix jobs (e.g., publish).
    #[must_use]
    pub fn build_artifact_aggregation_job(
        &self,
        task: &Task,
        stages: &StageConfiguration,
        environment: Option<&String>,
        previous_jobs: &[String],
    ) -> Job {
        let mut steps = Vec::new();

        // Checkout with full history for releases
        steps.push(
            Step::uses("actions/checkout@v4")
                .with_name("Checkout")
                .with_input("fetch-depth", serde_yaml::Value::Number(0.into())),
        );

        // Render bootstrap and setup stages using the StageRenderer
        let (stage_steps, secret_env_vars) = Self::render_stage_steps(stages);
        steps.extend(stage_steps);

        // Download artifacts from previous jobs
        for artifact in &task.artifact_downloads {
            // Find matching jobs based on artifact name pattern
            for prev_job in previous_jobs {
                // Check if this job matches the artifact source pattern
                let source_prefix = artifact.name.replace('.', "-");
                if prev_job.starts_with(&source_prefix) || prev_job.contains(&artifact.name) {
                    // Extract the arch/variant suffix from the job name
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

        // Build task command with --skip-dependencies
        let task_command = environment.map_or_else(
            || format!("cuenv task {} --skip-dependencies", task.id),
            |env| format!("cuenv task {} -e {} --skip-dependencies", task.id, env),
        );

        let mut task_step = Step::run(&task_command)
            .with_name(task.id.clone())
            .with_env("GITHUB_TOKEN", "${{ secrets.GITHUB_TOKEN }}");

        // Add params as environment variables
        for (key, value) in &task.params {
            task_step.env.insert(key.to_uppercase(), value.clone());
        }

        // Add secret env vars from setup stages to the task step
        for (key, value) in secret_env_vars {
            task_step.env.insert(key, transform_secret_ref(&value));
        }

        steps.push(task_step);

        Job {
            name: Some(task.id.clone()),
            runs_on: RunsOn::Label(self.runner.clone()),
            needs: previous_jobs.to_vec(),
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
    /// * `stages` - Stage configuration for bootstrap/setup steps
    /// * `environment` - Optional environment name for the task
    /// * `arch_runners` - Optional mapping of arch -> runner label
    /// * `previous_jobs` - Jobs that must complete before these matrix jobs
    #[must_use]
    pub fn build_matrix_jobs(
        &self,
        task: &Task,
        stages: &StageConfiguration,
        environment: Option<&String>,
        arch_runners: Option<&HashMap<String, String>>,
        previous_jobs: &[String],
    ) -> IndexMap<String, Job> {
        let mut jobs = IndexMap::new();
        let base_job_id = task.id.replace(['.', ' '], "-");

        let Some(matrix) = &task.matrix else {
            return jobs;
        };

        // Handle single-dimension matrix (arch) for now
        if let Some(arch_values) = matrix.dimensions.get("arch") {
            for arch in arch_values {
                let job_id = format!("{base_job_id}-{arch}");

                // Determine runner for this arch
                let runner = arch_runners
                    .and_then(|m| m.get(arch))
                    .cloned()
                    .unwrap_or_else(|| self.runner.clone());

                let mut steps = Vec::new();

                // Checkout with full history for releases
                steps.push(
                    Step::uses("actions/checkout@v4")
                        .with_name("Checkout")
                        .with_input("fetch-depth", serde_yaml::Value::Number(0.into())),
                );

                // Render bootstrap and setup stages using the StageRenderer
                let (stage_steps, secret_env_vars) = Self::render_stage_steps(stages);
                steps.extend(stage_steps);

                // Run the task with --skip-dependencies
                let task_command = environment.map_or_else(
                    || format!("cuenv task {} --skip-dependencies", task.id),
                    |env| format!("cuenv task {} -e {} --skip-dependencies", task.id, env),
                );
                let mut task_step = Step::run(&task_command)
                    .with_name(format!("{} ({arch})", task.id))
                    .with_env("GITHUB_TOKEN", "${{ secrets.GITHUB_TOKEN }}");

                // Add arch as an environment variable for the task
                task_step.env.insert("CUENV_ARCH".to_string(), arch.clone());

                // Add secret env vars from setup stages to the task step
                for (key, value) in &secret_env_vars {
                    task_step.env.insert(key.clone(), transform_secret_ref(value));
                }

                steps.push(task_step);

                // Upload artifact for matrix tasks (outputs from the build)
                let mut upload_step = Step::uses("actions/upload-artifact@v4")
                    .with_name("Upload artifacts")
                    .with_input(
                        "name",
                        serde_yaml::Value::String(format!("{base_job_id}-{arch}")),
                    )
                    .with_input(
                        "path",
                        serde_yaml::Value::String("result/bin/*".to_string()),
                    );
                upload_step.with_inputs.insert(
                    "if-no-files-found".to_string(),
                    serde_yaml::Value::String("ignore".to_string()),
                );
                steps.push(upload_step);

                jobs.insert(
                    job_id,
                    Job {
                        name: Some(format!("{} ({arch})", task.id)),
                        runs_on: RunsOn::Label(runner),
                        needs: previous_jobs.to_vec(),
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

impl Emitter for GitHubActionsEmitter {
    fn emit(&self, ir: &IntermediateRepresentation) -> EmitterResult<String> {
        let workflow_name = Self::build_workflow_name(ir);
        let workflow = self.build_workflow(ir, &workflow_name);
        Self::serialize_workflow(&workflow)
    }

    fn format_name(&self) -> &'static str {
        "github"
    }

    fn file_extension(&self) -> &'static str {
        "yml"
    }

    fn description(&self) -> &'static str {
        "GitHub Actions workflow YAML emitter"
    }

    fn validate(&self, ir: &IntermediateRepresentation) -> EmitterResult<()> {
        // Validate task IDs are valid job identifiers
        for task in &ir.tasks {
            if task.id.contains(' ') {
                return Err(EmitterError::InvalidIR(format!(
                    "Task ID '{}' contains spaces, which are not allowed in GitHub Actions job IDs",
                    task.id
                )));
            }
        }

        // Validate dependencies exist
        let task_ids: std::collections::HashSet<_> = ir.tasks.iter().map(|t| &t.id).collect();
        for task in &ir.tasks {
            for dep in &task.depends_on {
                if !task_ids.contains(dep) {
                    return Err(EmitterError::InvalidIR(format!(
                        "Task '{}' depends on non-existent task '{}'",
                        task.id, dep
                    )));
                }
            }
        }

        Ok(())
    }
}

/// Sanitize a string for use as a workflow filename
fn sanitize_filename(name: &str) -> String {
    name.to_lowercase()
        .replace(' ', "-")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// Sanitize a string for use as a job ID
fn sanitize_job_id(id: &str) -> String {
    id.replace(['.', ' '], "-")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// Target platform configuration for release builds.
#[derive(Debug, Clone)]
pub struct ReleaseTarget {
    /// Target identifier (e.g., "linux-x64")
    pub id: String,
    /// Rust target triple
    pub rust_triple: String,
    /// GitHub Actions runner
    pub runner: String,
}

impl ReleaseTarget {
    /// Default release targets: linux-x64, linux-arm64, darwin-arm64
    ///
    /// Uses the provided runner for Linux builds, falling back to "ubuntu-latest"
    /// if not specified.
    #[must_use]
    pub fn defaults_with_runner(linux_runner: Option<&str>) -> Vec<Self> {
        let linux = linux_runner.unwrap_or("ubuntu-latest").to_string();
        vec![
            Self {
                id: "linux-x64".to_string(),
                rust_triple: "x86_64-unknown-linux-gnu".to_string(),
                runner: linux.clone(),
            },
            Self {
                id: "linux-arm64".to_string(),
                rust_triple: "aarch64-unknown-linux-gnu".to_string(),
                runner: linux,
            },
            Self {
                id: "darwin-arm64".to_string(),
                rust_triple: "aarch64-apple-darwin".to_string(),
                runner: "macos-14".to_string(),
            },
        ]
    }

    /// Default release targets with ubuntu-latest for Linux builds.
    #[must_use]
    pub fn defaults() -> Vec<Self> {
        Self::defaults_with_runner(None)
    }
}

/// Builder for creating workflows with release matrix builds.
pub struct ReleaseWorkflowBuilder {
    emitter: GitHubActionsEmitter,
    targets: Vec<ReleaseTarget>,
}

impl ReleaseWorkflowBuilder {
    /// Create a new release workflow builder with default targets.
    ///
    /// Uses the emitter's configured runner for Linux builds.
    #[must_use]
    pub fn new(emitter: GitHubActionsEmitter) -> Self {
        let targets = ReleaseTarget::defaults_with_runner(Some(&emitter.runner));
        Self { emitter, targets }
    }

    /// Set custom release targets.
    #[must_use]
    pub fn with_targets(mut self, targets: Vec<ReleaseTarget>) -> Self {
        self.targets = targets;
        self
    }

    /// Build a release workflow with matrix build and publish jobs.
    #[must_use]
    pub fn build(&self, ir: &IntermediateRepresentation) -> Workflow {
        let workflow_name = GitHubActionsEmitter::build_workflow_name(ir);

        // Build triggers for release workflows
        let triggers = WorkflowTriggers {
            release: Some(ReleaseTrigger {
                types: vec!["published".to_string()],
            }),
            workflow_dispatch: Some(WorkflowDispatchTrigger {
                inputs: {
                    let mut inputs = IndexMap::new();
                    inputs.insert(
                        "tag_name".to_string(),
                        WorkflowInput {
                            description: "Tag to release (e.g., v0.16.0)".to_string(),
                            required: Some(true),
                            default: None,
                            input_type: Some("string".to_string()),
                            options: None,
                        },
                    );
                    inputs
                },
            }),
            ..Default::default()
        };

        // Build release-specific jobs
        let mut jobs = IndexMap::new();
        jobs.insert("build".to_string(), self.build_matrix_job(ir));
        jobs.insert("publish".to_string(), self.build_publish_job(ir));

        Workflow {
            name: workflow_name,
            on: triggers,
            concurrency: Some(Concurrency {
                group: "${{ github.workflow }}-${{ github.head_ref || github.ref }}".to_string(),
                cancel_in_progress: Some(true),
            }),
            permissions: Some(Permissions {
                contents: Some(PermissionLevel::Write),
                id_token: Some(PermissionLevel::Write),
                ..Default::default()
            }),
            env: IndexMap::new(),
            jobs,
        }
    }

    /// Build the matrix build job.
    fn build_matrix_job(&self, ir: &IntermediateRepresentation) -> Job {
        // Create matrix include entries for each target
        let matrix_include: Vec<IndexMap<String, serde_yaml::Value>> = self
            .targets
            .iter()
            .map(|t| {
                let mut entry = IndexMap::new();
                entry.insert(
                    "target".to_string(),
                    serde_yaml::Value::String(t.id.clone()),
                );
                entry.insert(
                    "rust-triple".to_string(),
                    serde_yaml::Value::String(t.rust_triple.clone()),
                );
                entry.insert(
                    "runs-on".to_string(),
                    serde_yaml::Value::String(t.runner.clone()),
                );
                entry
            })
            .collect();

        // Build steps
        let mut steps = Vec::new();

        // Checkout
        steps.push(
            Step::uses("actions/checkout@v4")
                .with_name("Checkout")
                .with_input("fetch-depth", serde_yaml::Value::Number(0.into())),
        );

        // Check IR stages for Nix setup
        let has_install_nix = ir.stages.bootstrap.iter().any(|t| t.id == "install-nix");
        if has_install_nix {
            steps.push(
                Step::uses("DeterminateSystems/nix-installer-action@v16")
                    .with_name("Install Nix")
                    .with_input(
                        "extra-conf",
                        serde_yaml::Value::String("accept-flake-config = true".to_string()),
                    ),
            );
        }

        // Setup cuenv
        if let Some(cuenv_task) = ir.stages.setup.iter().find(|t| t.id == "setup-cuenv") {
            let command = cuenv_task.command.first().cloned().unwrap_or_default();
            steps.push(Step::run(&command).with_name("Setup cuenv"));
        }

        // Build for target
        let environment = ir.pipeline.environment.as_deref();
        let build_cmd = environment.map_or_else(
            || "cuenv release binaries --build-only --target ${{ matrix.target }}".to_string(),
            |env| {
                "cuenv release binaries --build-only --target ${{ matrix.target }} -e $ENV"
                    .replace("$ENV", env)
            },
        );
        steps.push(Step::run(&build_cmd).with_name("Build for ${{ matrix.target }}"));

        // Upload artifact
        let mut upload_step = Step::uses("actions/upload-artifact@v4")
            .with_name("Upload binary")
            .with_input(
                "name",
                serde_yaml::Value::String("binary-${{ matrix.target }}".to_string()),
            )
            .with_input(
                "path",
                serde_yaml::Value::String("target/${{ matrix.rust-triple }}/release/*".to_string()),
            );
        upload_step.with_inputs.insert(
            "if-no-files-found".to_string(),
            serde_yaml::Value::String("error".to_string()),
        );
        steps.push(upload_step);

        Job {
            name: Some("Build ${{ matrix.target }}".to_string()),
            runs_on: RunsOn::Label("${{ matrix.runs-on }}".to_string()),
            needs: Vec::new(),
            if_condition: None,
            strategy: Some(Strategy {
                matrix: Matrix {
                    include: matrix_include,
                },
                fail_fast: Some(false),
                max_parallel: None,
            }),
            environment: None,
            env: IndexMap::new(),
            concurrency: None,
            continue_on_error: None,
            timeout_minutes: Some(60),
            steps,
        }
    }

    /// Build the publish job that runs after all builds complete.
    fn build_publish_job(&self, ir: &IntermediateRepresentation) -> Job {
        let mut steps = Vec::new();

        // Checkout
        steps.push(
            Step::uses("actions/checkout@v4")
                .with_name("Checkout")
                .with_input("fetch-depth", serde_yaml::Value::Number(0.into())),
        );

        // Check IR stages for Nix setup
        let has_install_nix = ir.stages.bootstrap.iter().any(|t| t.id == "install-nix");
        if has_install_nix {
            steps.push(
                Step::uses("DeterminateSystems/nix-installer-action@v16")
                    .with_name("Install Nix")
                    .with_input(
                        "extra-conf",
                        serde_yaml::Value::String("accept-flake-config = true".to_string()),
                    ),
            );
        }

        // Setup cuenv
        if let Some(cuenv_task) = ir.stages.setup.iter().find(|t| t.id == "setup-cuenv") {
            let command = cuenv_task.command.first().cloned().unwrap_or_default();
            steps.push(Step::run(&command).with_name("Setup cuenv"));
        }

        // Download all artifacts
        for target in &self.targets {
            let mut download_step = Step::uses("actions/download-artifact@v4")
                .with_name(format!("Download {}", target.id))
                .with_input(
                    "name",
                    serde_yaml::Value::String(format!("binary-{}", target.id)),
                )
                .with_input(
                    "path",
                    serde_yaml::Value::String(format!("target/{}/release", target.rust_triple)),
                );
            download_step.continue_on_error = Some(false);
            steps.push(download_step);
        }

        // Setup 1Password if needed
        let has_1password = ir.stages.setup.iter().any(|t| t.id == "setup-1password");
        if has_1password {
            steps.push(Step::run("cuenv secrets setup onepassword").with_name("Setup 1Password"));
        }

        // Run publish
        let environment = ir.pipeline.environment.as_deref();
        let publish_cmd = environment.map_or_else(
            || "cuenv release binaries --publish-only".to_string(),
            |env| format!("cuenv release binaries --publish-only -e {env}"),
        );
        let mut publish_step = Step::run(&publish_cmd)
            .with_name("Publish release")
            .with_env("GITHUB_TOKEN", "${{ secrets.GITHUB_TOKEN }}");

        if has_1password {
            publish_step.env.insert(
                "OP_SERVICE_ACCOUNT_TOKEN".to_string(),
                "${{ secrets.OP_SERVICE_ACCOUNT_TOKEN }}".to_string(),
            );
        }
        steps.push(publish_step);

        Job {
            name: Some("Publish Release".to_string()),
            runs_on: RunsOn::Label(self.emitter.runner.clone()),
            needs: vec!["build".to_string()],
            if_condition: None,
            strategy: None,
            environment: Some(Environment::Name(
                ir.pipeline
                    .environment
                    .clone()
                    .unwrap_or_else(|| "production".to_string()),
            )),
            env: IndexMap::new(),
            concurrency: None,
            continue_on_error: None,
            timeout_minutes: Some(30),
            steps,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_ci::ir::{
        CachePolicy, PipelineMetadata, ResourceRequirements, StageConfiguration, StageTask,
    };

    fn make_ir(tasks: Vec<Task>) -> IntermediateRepresentation {
        IntermediateRepresentation {
            version: "1.4".to_string(),
            pipeline: PipelineMetadata {
                name: "test-pipeline".to_string(),
                environment: None,
                requires_onepassword: false,
                project_name: None,
                trigger: None,
                pipeline_tasks: vec![],
            },
            runtimes: vec![],
            stages: StageConfiguration::default(),
            tasks,
        }
    }

    fn make_task(id: &str, command: &[&str]) -> Task {
        Task {
            id: id.to_string(),
            runtime: None,
            command: command.iter().map(|s| (*s).to_string()).collect(),
            shell: false,
            env: HashMap::new(),
            secrets: HashMap::new(),
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
            params: HashMap::new(),
        }
    }

    #[test]
    fn test_simple_workflow() {
        let emitter = GitHubActionsEmitter::new()
            .without_nix()
            .without_cuenv_build();
        let ir = make_ir(vec![make_task("build", &["cargo", "build"])]);

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.contains("name: test-pipeline"));
        assert!(yaml.contains("jobs:"));
        assert!(yaml.contains("build:"));
        assert!(yaml.contains("cuenv task build"));
    }

    #[test]
    fn test_workflow_with_nix() {
        let emitter = GitHubActionsEmitter::new().with_nix();
        let mut ir = make_ir(vec![make_task("build", &["cargo", "build"])]);

        // Add stage tasks that would be contributed by NixContributor
        ir.stages.bootstrap.push(StageTask {
            id: "install-nix".to_string(),
            provider: "nix".to_string(),
            label: Some("Install Nix".to_string()),
            command: vec!["curl ... | sh".to_string()],
            shell: true,
            priority: 0,
            ..Default::default()
        });
        ir.stages.setup.push(StageTask {
            id: "setup-cuenv".to_string(),
            provider: "cuenv".to_string(),
            label: Some("Setup cuenv".to_string()),
            command: vec!["nix build .#cuenv".to_string()],
            shell: true,
            depends_on: vec!["install-nix".to_string()],
            priority: 10,
            ..Default::default()
        });

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.contains("DeterminateSystems/nix-installer-action"));
        assert!(yaml.contains("nix build .#cuenv"));
    }

    #[test]
    fn test_workflow_with_cachix() {
        let emitter = GitHubActionsEmitter::new()
            .with_nix()
            .with_cachix("my-cache");
        let mut ir = make_ir(vec![make_task("build", &["cargo", "build"])]);

        // Add stage tasks for Cachix
        ir.stages.bootstrap.push(StageTask {
            id: "install-nix".to_string(),
            provider: "nix".to_string(),
            label: Some("Install Nix".to_string()),
            command: vec!["curl ... | sh".to_string()],
            shell: true,
            priority: 0,
            ..Default::default()
        });
        let mut env = HashMap::new();
        env.insert(
            "CACHIX_AUTH_TOKEN".to_string(),
            "${CACHIX_AUTH_TOKEN}".to_string(),
        );
        ir.stages.setup.push(StageTask {
            id: "setup-cachix".to_string(),
            provider: "cachix".to_string(),
            label: Some("Setup Cachix".to_string()),
            command: vec!["nix-env -iA cachix && cachix use my-cache".to_string()],
            shell: true,
            env,
            depends_on: vec!["install-nix".to_string()],
            priority: 5,
            ..Default::default()
        });

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.contains("cachix/cachix-action"));
        assert!(yaml.contains("name: my-cache"));
    }

    #[test]
    fn test_workflow_with_dependencies() {
        let emitter = GitHubActionsEmitter::new()
            .without_nix()
            .without_cuenv_build();
        let mut test_task = make_task("test", &["cargo", "test"]);
        test_task.depends_on = vec!["build".to_string()];

        let ir = make_ir(vec![make_task("build", &["cargo", "build"]), test_task]);

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.contains("needs:"));
        assert!(yaml.contains("- build"));
    }

    #[test]
    fn test_workflow_with_manual_approval() {
        let emitter = GitHubActionsEmitter::new()
            .without_nix()
            .without_cuenv_build()
            .with_approval_environment("staging");
        let mut deploy_task = make_task("deploy", &["./deploy.sh"]);
        deploy_task.manual_approval = true;

        let ir = make_ir(vec![deploy_task]);

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.contains("environment: staging"));
    }

    #[test]
    fn test_workflow_with_concurrency_group() {
        let emitter = GitHubActionsEmitter::new()
            .without_nix()
            .without_cuenv_build();
        let mut deploy_task = make_task("deploy", &["./deploy.sh"]);
        deploy_task.concurrency_group = Some("production".to_string());

        let ir = make_ir(vec![deploy_task]);

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.contains("concurrency:"));
        assert!(yaml.contains("group: production"));
    }

    #[test]
    fn test_workflow_with_custom_runner() {
        let emitter = GitHubActionsEmitter::new()
            .without_nix()
            .without_cuenv_build()
            .with_runner("self-hosted");
        let ir = make_ir(vec![make_task("build", &["cargo", "build"])]);

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.contains("runs-on: self-hosted"));
    }

    #[test]
    fn test_workflow_with_resource_tags() {
        let emitter = GitHubActionsEmitter::new()
            .without_nix()
            .without_cuenv_build();
        let mut task = make_task("build", &["cargo", "build"]);
        task.resources = Some(ResourceRequirements {
            cpu: None,
            memory: None,
            tags: vec!["blacksmith-8vcpu-ubuntu-2404".to_string()],
        });

        let ir = make_ir(vec![task]);

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.contains("runs-on: blacksmith-8vcpu-ubuntu-2404"));
    }

    #[test]
    fn test_emit_workflows() {
        let emitter = GitHubActionsEmitter::new()
            .without_nix()
            .without_cuenv_build();
        let ir = make_ir(vec![make_task("build", &["cargo", "build"])]);

        let workflows = emitter.emit_workflows(&ir).unwrap();

        assert_eq!(workflows.len(), 1);
        assert!(workflows.contains_key("test-pipeline.yml"));
    }

    #[test]
    fn test_sanitize_filename() {
        assert_eq!(sanitize_filename("CI Pipeline"), "ci-pipeline");
        assert_eq!(sanitize_filename("release/v1"), "releasev1");
        assert_eq!(sanitize_filename("test_workflow"), "test_workflow");
    }

    #[test]
    fn test_sanitize_job_id() {
        assert_eq!(sanitize_job_id("build.test"), "build-test");
        assert_eq!(sanitize_job_id("deploy prod"), "deploy-prod");
    }

    #[test]
    fn test_validation_invalid_id() {
        let emitter = GitHubActionsEmitter::new();
        let ir = make_ir(vec![make_task("invalid task", &["echo"])]);

        let result = emitter.validate(&ir);
        assert!(result.is_err());
    }

    #[test]
    fn test_validation_missing_dependency() {
        let emitter = GitHubActionsEmitter::new();
        let mut task = make_task("test", &["cargo", "test"]);
        task.depends_on = vec!["nonexistent".to_string()];

        let ir = make_ir(vec![task]);

        let result = emitter.validate(&ir);
        assert!(result.is_err());
    }

    #[test]
    fn test_format_name() {
        let emitter = GitHubActionsEmitter::new();
        assert_eq!(emitter.format_name(), "github");
        assert_eq!(emitter.file_extension(), "yml");
    }

    #[test]
    fn test_generation_header() {
        let emitter = GitHubActionsEmitter::new()
            .without_nix()
            .without_cuenv_build();
        let ir = make_ir(vec![make_task("build", &["cargo", "build"])]);

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.starts_with("# Generated by cuenv"));
        assert!(yaml.contains("cuenv ci --format github"));
    }

    // =========================================================================
    // Tests for new matrix/artifact job building methods
    // =========================================================================

    #[test]
    fn test_build_simple_job() {
        let emitter = GitHubActionsEmitter::new().with_runner("ubuntu-latest");
        let task = make_task("build", &["cargo", "build"]);
        let stages = StageConfiguration::default();

        let job = emitter.build_simple_job(&task, &stages, None);

        assert_eq!(job.name, Some("build".to_string()));
        assert!(matches!(job.runs_on, RunsOn::Label(ref l) if l == "ubuntu-latest"));
        assert!(job.needs.is_empty()); // Caller sets needs
        assert!(!job.steps.is_empty());

        // Should have checkout and task run steps
        let step_names: Vec<_> = job.steps.iter().filter_map(|s| s.name.as_ref()).collect();
        assert!(step_names.contains(&&"Checkout".to_string()));
        assert!(step_names.contains(&&"build".to_string()));
    }

    #[test]
    fn test_build_simple_job_with_environment() {
        let emitter = GitHubActionsEmitter::new();
        let task = make_task("deploy", &["./deploy.sh"]);
        let stages = StageConfiguration::default();
        let env = "production".to_string();

        let job = emitter.build_simple_job(&task, &stages, Some(&env));

        // Find the task step and check command includes environment
        let task_step = job.steps.iter().find(|s| s.name.as_deref() == Some("deploy"));
        assert!(task_step.is_some());
        let run_cmd = task_step.unwrap().run.as_ref().unwrap();
        assert!(run_cmd.contains("-e production"));
        assert!(run_cmd.contains("--skip-dependencies"));
    }

    #[test]
    fn test_build_matrix_jobs() {
        use cuenv_ci::ir::MatrixConfig;

        let emitter = GitHubActionsEmitter::new().with_runner("ubuntu-latest");
        let mut task = make_task("release.build", &["cargo", "build"]);
        task.matrix = Some(MatrixConfig {
            dimensions: [("arch".to_string(), vec!["linux-x64".to_string(), "darwin-arm64".to_string()])]
                .into_iter()
                .collect(),
            ..Default::default()
        });
        let stages = StageConfiguration::default();

        let jobs = emitter.build_matrix_jobs(&task, &stages, None, None, &[]);

        // Should create 2 jobs, one per arch
        assert_eq!(jobs.len(), 2);
        assert!(jobs.contains_key("release-build-linux-x64"));
        assert!(jobs.contains_key("release-build-darwin-arm64"));

        // Each job should have the arch in its name
        let linux_job = jobs.get("release-build-linux-x64").unwrap();
        assert_eq!(linux_job.name, Some("release.build (linux-x64)".to_string()));

        // Should have CUENV_ARCH env var
        let task_step = linux_job.steps.iter().find(|s| s.name.as_deref() == Some("release.build (linux-x64)"));
        assert!(task_step.is_some());
        assert_eq!(task_step.unwrap().env.get("CUENV_ARCH"), Some(&"linux-x64".to_string()));
    }

    #[test]
    fn test_build_matrix_jobs_with_arch_runners() {
        use cuenv_ci::ir::MatrixConfig;

        let emitter = GitHubActionsEmitter::new().with_runner("ubuntu-latest");
        let mut task = make_task("build", &["cargo", "build"]);
        task.matrix = Some(MatrixConfig {
            dimensions: [("arch".to_string(), vec!["linux-x64".to_string(), "darwin-arm64".to_string()])]
                .into_iter()
                .collect(),
            ..Default::default()
        });
        let stages = StageConfiguration::default();
        let arch_runners: HashMap<String, String> = [
            ("linux-x64".to_string(), "ubuntu-24.04".to_string()),
            ("darwin-arm64".to_string(), "macos-14".to_string()),
        ]
        .into_iter()
        .collect();

        let jobs = emitter.build_matrix_jobs(&task, &stages, None, Some(&arch_runners), &[]);

        // Check runners are correctly mapped
        let linux_job = jobs.get("build-linux-x64").unwrap();
        assert!(matches!(linux_job.runs_on, RunsOn::Label(ref l) if l == "ubuntu-24.04"));

        let darwin_job = jobs.get("build-darwin-arm64").unwrap();
        assert!(matches!(darwin_job.runs_on, RunsOn::Label(ref l) if l == "macos-14"));
    }

    #[test]
    fn test_build_artifact_aggregation_job() {
        use cuenv_ci::ir::ArtifactDownload;

        let emitter = GitHubActionsEmitter::new();
        let mut task = make_task("release.publish", &["./publish.sh"]);
        task.artifact_downloads = vec![
            ArtifactDownload {
                name: "release-build".to_string(),
                path: "./artifacts".to_string(),
                filter: String::new(),
            },
        ];
        task.params = [("version".to_string(), "1.0.0".to_string())]
            .into_iter()
            .collect();
        let stages = StageConfiguration::default();
        let previous_jobs = vec!["release-build-linux-x64".to_string(), "release-build-darwin-arm64".to_string()];

        let job = emitter.build_artifact_aggregation_job(&task, &stages, None, &previous_jobs);

        assert_eq!(job.name, Some("release.publish".to_string()));
        assert_eq!(job.needs, previous_jobs);
        assert_eq!(job.timeout_minutes, Some(30));

        // Should have download artifact steps
        let download_steps: Vec<_> = job.steps.iter()
            .filter(|s| s.uses.as_deref() == Some("actions/download-artifact@v4"))
            .collect();
        assert_eq!(download_steps.len(), 2);

        // Task step should have params as env vars
        let task_step = job.steps.iter().find(|s| s.name.as_deref() == Some("release.publish"));
        assert!(task_step.is_some());
        assert_eq!(task_step.unwrap().env.get("VERSION"), Some(&"1.0.0".to_string()));
    }

    #[test]
    fn test_task_has_matrix() {
        use cuenv_ci::ir::MatrixConfig;

        let task_without = make_task("build", &["cargo", "build"]);
        assert!(!GitHubActionsEmitter::task_has_matrix(&task_without));

        let mut task_with_empty = make_task("build", &["cargo", "build"]);
        task_with_empty.matrix = Some(MatrixConfig::default());
        assert!(!GitHubActionsEmitter::task_has_matrix(&task_with_empty));

        let mut task_with_matrix = make_task("build", &["cargo", "build"]);
        task_with_matrix.matrix = Some(MatrixConfig {
            dimensions: [("arch".to_string(), vec!["x64".to_string()])]
                .into_iter()
                .collect(),
            ..Default::default()
        });
        assert!(GitHubActionsEmitter::task_has_matrix(&task_with_matrix));
    }

    #[test]
    fn test_task_has_artifact_downloads() {
        use cuenv_ci::ir::ArtifactDownload;

        let task_without = make_task("build", &["cargo", "build"]);
        assert!(!GitHubActionsEmitter::task_has_artifact_downloads(&task_without));

        let mut task_with = make_task("publish", &["./publish.sh"]);
        task_with.artifact_downloads = vec![
            ArtifactDownload {
                name: "build".to_string(),
                path: "./out".to_string(),
                filter: String::new(),
            },
        ];
        assert!(GitHubActionsEmitter::task_has_artifact_downloads(&task_with));
    }

    #[test]
    fn test_render_stage_steps() {
        let mut stages = StageConfiguration::default();
        stages.bootstrap.push(StageTask {
            id: "install-nix".to_string(),
            provider: "nix".to_string(),
            label: Some("Install Nix".to_string()),
            command: vec!["curl ... | sh".to_string()],
            shell: true,
            ..Default::default()
        });
        stages.setup.push(StageTask {
            id: "setup-cuenv".to_string(),
            provider: "cuenv".to_string(),
            label: Some("Setup cuenv".to_string()),
            command: vec!["nix build .#cuenv".to_string()],
            env: [("MY_VAR".to_string(), "${MY_SECRET}".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        });

        let (steps, secret_env_vars) = GitHubActionsEmitter::render_stage_steps(&stages);

        assert_eq!(steps.len(), 2);
        assert!(steps[0].name.as_deref() == Some("Install Nix"));
        assert!(steps[1].name.as_deref() == Some("Setup cuenv"));

        // Secret env vars should be collected
        assert_eq!(secret_env_vars.get("MY_VAR"), Some(&"${MY_SECRET}".to_string()));
    }
}
