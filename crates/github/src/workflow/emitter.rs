//! GitHub Actions Workflow Emitter
//!
//! Transforms cuenv IR into GitHub Actions workflow YAML files.

use crate::workflow::schema::{
    Concurrency, Environment, Job, PermissionLevel, Permissions, PullRequestTrigger, PushTrigger,
    ReleaseTrigger, RunsOn, ScheduleTrigger, Step, Workflow, WorkflowDispatchTrigger,
    WorkflowInput, WorkflowTriggers,
};
use crate::workflow::stage_renderer::{GitHubStageRenderer, transform_secret_ref};
use cuenv_ci::emitter::{Emitter, EmitterError, EmitterResult};
use cuenv_ci::ir::{BuildStage, IntermediateRepresentation, OutputType, TriggerCondition};
use indexmap::IndexMap;
use std::collections::HashMap;

pub use super::jobs::{
    ArtifactAggregationJobOptions, CuenvBootstrapJobOptions, CuenvSetup, MatrixJobOptions,
    SimpleJobOptions, TaskExecution,
};
pub use super::release::{ReleaseTarget, ReleaseWorkflowBuilder};

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
/// | `task.command` | Step with `run: cuenv task {task.id}` or the direct IR command |
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
    /// Include cuenv build step (via nix build)
    pub build_cuenv: bool,
    /// Environment name for manual approval tasks
    pub approval_environment: String,
    /// Configured permissions from the manifest
    pub configured_permissions: HashMap<String, String>,
}

impl Default for GitHubActionsEmitter {
    fn default() -> Self {
        Self {
            runner: "ubuntu-latest".to_string(),
            use_nix: true,
            use_cachix: false,
            cachix_name: None,
            cachix_auth_token_secret: "CACHIX_AUTH_TOKEN".to_string(),
            build_cuenv: true,
            approval_environment: "production".to_string(),
            configured_permissions: HashMap::new(),
        }
    }
}

impl GitHubActionsEmitter {
    /// Create a new GitHub Actions emitter with default settings
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn stage_renderer(&self) -> GitHubStageRenderer {
        let mut renderer = GitHubStageRenderer::new()
            .with_cachix_auth_token_secret(self.cachix_auth_token_secret.clone());
        if let Some(name) = &self.cachix_name {
            renderer = renderer.with_cachix(name.clone());
        }
        renderer
    }

    pub(crate) fn add_github_context_env(step: &mut Step) {
        for (key, value) in [
            ("GITHUB_TOKEN", "${{ secrets.GITHUB_TOKEN }}"),
            ("GITHUB_ACTOR", "${{ github.actor }}"),
            ("GITHUB_REF_TYPE", "${{ github.ref_type }}"),
            ("GITHUB_REF_NAME", "${{ github.ref_name }}"),
        ] {
            step.env
                .entry(key.to_string())
                .or_insert_with(|| value.to_string());
        }
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

        // Apply configured permissions
        if let Some(permissions) = &config.permissions {
            emitter.configured_permissions.clone_from(permissions);
        }

        emitter
    }

    /// Get the configured runner as a `RunsOn` value
    #[must_use]
    pub fn runner_as_runs_on(&self) -> RunsOn {
        RunsOn::Label(self.runner.clone())
    }

    /// Apply configured permissions to a base Permissions struct
    #[must_use]
    pub fn apply_configured_permissions(&self, mut permissions: Permissions) -> Permissions {
        // Helper to parse permission level from string
        let parse_level = |s: &str| -> Option<PermissionLevel> {
            match s.to_lowercase().as_str() {
                "write" => Some(PermissionLevel::Write),
                "read" => Some(PermissionLevel::Read),
                "none" => Some(PermissionLevel::None),
                _ => None,
            }
        };

        // Apply configured permissions (override calculated)
        for (key, value) in &self.configured_permissions {
            if let Some(level) = parse_level(value) {
                match key.as_str() {
                    "contents" => permissions.contents = Some(level),
                    "checks" => permissions.checks = Some(level),
                    "pull-requests" => permissions.pull_requests = Some(level),
                    "issues" => permissions.issues = Some(level),
                    "packages" => permissions.packages = Some(level),
                    "id-token" => permissions.id_token = Some(level),
                    "actions" => permissions.actions = Some(level),
                    _ => {}
                }
            }
        }

        permissions
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

    /// Emit a thin mode workflow, returning the filename and YAML content.
    ///
    /// This is the primary entry point for thin workflow generation. It builds
    /// a single-job workflow that delegates task execution to `cuenv ci`.
    ///
    /// # Errors
    ///
    /// Returns `EmitterError::Serialization` if YAML serialization fails.
    pub fn emit_thin_workflow(
        &self,
        ir: &IntermediateRepresentation,
    ) -> EmitterResult<(String, String)> {
        let workflow_name = Self::build_workflow_name(ir);
        let filename = format!("{}.yml", sanitize_filename(&workflow_name));
        let yaml = self.emit_thin(ir)?;
        Ok((filename, yaml))
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
    pub(crate) fn build_workflow_name(ir: &IntermediateRepresentation) -> String {
        ir.pipeline.project_name.as_ref().map_or_else(
            || ir.pipeline.name.clone(),
            |project| format!("{}-{}", project, ir.pipeline.name),
        )
    }

    /// Build a workflow from the IR
    fn build_workflow(&self, ir: &IntermediateRepresentation, workflow_name: &str) -> Workflow {
        let workflow_filename = format!("{}.yml", sanitize_filename(workflow_name));
        let triggers = self.build_triggers(ir, &workflow_filename);
        let permissions = self.build_permissions(ir);
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
    #[must_use]
    pub fn build_triggers(
        &self,
        ir: &IntermediateRepresentation,
        workflow_filename: &str,
    ) -> WorkflowTriggers {
        let trigger = ir.pipeline.trigger.as_ref();

        WorkflowTriggers {
            push: Self::build_push_trigger(trigger, workflow_filename),
            pull_request: Self::build_pr_trigger(trigger, workflow_filename),
            release: Self::build_release_trigger(trigger),
            workflow_dispatch: Self::build_manual_trigger(trigger),
            schedule: Self::build_schedule_trigger(trigger),
        }
    }

    /// Build push trigger from IR trigger condition
    fn build_push_trigger(
        trigger: Option<&TriggerCondition>,
        workflow_filename: &str,
    ) -> Option<PushTrigger> {
        let trigger = trigger?;

        // Only emit push trigger if we have branch conditions
        if trigger.branches.is_empty() {
            return None;
        }

        let paths = Self::build_trigger_paths(&trigger.paths, workflow_filename);

        Some(PushTrigger {
            branches: trigger.branches.clone(),
            paths,
            ..Default::default()
        })
    }

    /// Build pull request trigger from IR trigger condition
    fn build_pr_trigger(
        trigger: Option<&TriggerCondition>,
        workflow_filename: &str,
    ) -> Option<PullRequestTrigger> {
        let trigger = trigger?;

        // Only emit PR trigger if explicitly enabled - never default to running on PRs
        if trigger.pull_request == Some(true) {
            let paths = Self::build_trigger_paths(&trigger.paths, workflow_filename);

            Some(PullRequestTrigger {
                branches: trigger.branches.clone(),
                paths,
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

    /// Build trigger paths, adding the workflow file itself when path filtering is active.
    ///
    /// When a workflow has path-based triggers (e.g., only trigger on changes to `src/**`),
    /// this ensures the workflow also triggers when its own definition file changes.
    fn build_trigger_paths(paths: &[String], workflow_filename: &str) -> Vec<String> {
        if paths.is_empty() {
            return Vec::new();
        }

        let workflow_path = format!(".github/workflows/{workflow_filename}");

        if paths.contains(&workflow_path) {
            return paths.to_vec();
        }

        let mut result = paths.to_vec();
        result.push(workflow_path);
        result.sort();
        result
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

    /// Build permissions based on task requirements and configured permissions
    #[must_use]
    pub fn build_permissions(&self, ir: &IntermediateRepresentation) -> Permissions {
        let has_deployments = ir.tasks.iter().any(|t| t.deployment);
        let has_outputs = ir.tasks.iter().any(|t| {
            t.outputs
                .iter()
                .any(|o| o.output_type == OutputType::Orchestrator)
        });

        // Helper to parse permission level from string
        let parse_level = |s: &str| -> Option<PermissionLevel> {
            match s.to_lowercase().as_str() {
                "write" => Some(PermissionLevel::Write),
                "read" => Some(PermissionLevel::Read),
                "none" => Some(PermissionLevel::None),
                _ => None,
            }
        };

        // Start with calculated permissions
        let mut permissions = Permissions {
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
        };

        // Apply configured permissions (override calculated)
        for (key, value) in &self.configured_permissions {
            if let Some(level) = parse_level(value) {
                match key.as_str() {
                    "contents" => permissions.contents = Some(level),
                    "checks" => permissions.checks = Some(level),
                    "pull-requests" => permissions.pull_requests = Some(level),
                    "issues" => permissions.issues = Some(level),
                    "packages" => permissions.packages = Some(level),
                    "id-token" => permissions.id_token = Some(level),
                    "actions" => permissions.actions = Some(level),
                    _ => {}
                }
            }
        }

        permissions
    }

    /// Build jobs from IR tasks
    ///
    /// Uses phase tasks for bootstrap and setup steps, respecting `depends_on`
    /// relationships for correct step ordering.
    fn build_jobs(&self, ir: &IntermediateRepresentation) -> IndexMap<String, Job> {
        let mut jobs = IndexMap::new();

        for task in ir.regular_tasks() {
            // Build base job using phase tasks for bootstrap/setup
            let mut job = self.build_simple_job(
                task,
                ir,
                &SimpleJobOptions::orchestrated(
                    ir.pipeline.environment.as_ref(),
                    None, // project_path - not used in single-project mode
                ),
            );

            // Apply additional job configuration from task

            // Determine runner from task resources or use default
            if let Some(resources) = &task.resources
                && let Some(tag) = resources.tags.first()
            {
                job.runs_on = RunsOn::Label(tag.clone());
            }

            // Map task dependencies to sanitized job IDs
            job.needs = task.depends_on.iter().map(|d| sanitize_job_id(d)).collect();

            // Handle manual approval via environment
            if task.manual_approval {
                job.environment = Some(Environment::Name(self.approval_environment.clone()));
            }

            // Handle concurrency group
            if let Some(group) = &task.concurrency_group {
                job.concurrency = Some(Concurrency {
                    group: group.clone(),
                    cancel_in_progress: Some(false),
                });
            }

            jobs.insert(sanitize_job_id(&task.id), job);
        }

        jobs
    }

    /// Serialize a workflow to YAML with a generation header
    fn serialize_workflow(workflow: &Workflow) -> EmitterResult<String> {
        let yaml = serde_yaml::to_string(workflow)
            .map_err(|e| EmitterError::Serialization(e.to_string()))?;

        // Add generation header
        let header =
            "# Generated by cuenv - do not edit manually\n# Regenerate with: cuenv sync ci\n\n";

        Ok(format!("{header}{yaml}"))
    }
}

impl Emitter for GitHubActionsEmitter {
    /// Emit a thin mode GitHub Actions workflow.
    ///
    /// Thin mode generates a single-job workflow that:
    /// 1. Runs bootstrap phase steps (e.g., install Nix)
    /// 2. Runs setup phase steps (e.g., build cuenv)
    /// 3. Executes `cuenv ci --pipeline <name>` for orchestration
    /// 4. Runs success/failure phase steps with conditions
    fn emit_thin(&self, ir: &IntermediateRepresentation) -> EmitterResult<String> {
        use crate::workflow::stage_renderer::GitHubStageRenderer;

        let workflow_name = Self::build_workflow_name(ir);
        let workflow_filename = format!("{}.yml", sanitize_filename(&workflow_name));
        let triggers = self.build_triggers(ir, &workflow_filename);
        let permissions = self.build_permissions(ir);

        let mut renderer = GitHubStageRenderer::new()
            .with_cachix_auth_token_secret(self.cachix_auth_token_secret.clone());
        if let Some(name) = &self.cachix_name {
            renderer = renderer.with_cachix(name.clone());
        }
        let mut steps = Vec::new();

        // Checkout step
        steps.push(
            Step::uses("actions/checkout@v4")
                .with_name("Checkout")
                .with_input("fetch-depth", serde_yaml::Value::Number(2.into())),
        );

        // Bootstrap and setup phase steps
        let (phase_steps, secret_env) =
            self.render_phase_steps(ir, TaskExecution::Orchestrated, &CuenvSetup::BuildInJob);
        steps.extend(phase_steps);

        // Main execution step: cuenv ci --pipeline <name>
        let pipeline_name = &ir.pipeline.name;
        let cuenv_command = ir.pipeline.project_path.as_ref().map_or_else(
            || format!("cuenv ci --pipeline {pipeline_name}"),
            |path| format!("cuenv ci --pipeline {pipeline_name} --path {path}"),
        );

        let mut main_step =
            Step::run(&cuenv_command).with_name(format!("Run pipeline: {pipeline_name}"));
        Self::add_github_context_env(&mut main_step);

        if let Some(env) = &ir.pipeline.environment {
            main_step = main_step.with_env("CUENV_ENVIRONMENT", env.clone());
        }

        // Pass secret env vars from setup tasks to main step (e.g., OP_SERVICE_ACCOUNT_TOKEN)
        // Transform ${VAR} to ${{ secrets.VAR }} format for GitHub Actions
        for (key, value) in secret_env {
            main_step = main_step.with_env(key, transform_secret_ref(&value));
        }

        steps.push(main_step);

        // Success phase steps
        for task in ir.sorted_phase_tasks(BuildStage::Success) {
            let mut step = renderer.render_task(task);
            step.if_condition = Some("success()".to_string());
            steps.push(step);
        }

        // Failure phase steps
        for task in ir.sorted_phase_tasks(BuildStage::Failure) {
            let mut step = renderer.render_task(task);
            step.if_condition = Some("failure()".to_string());
            steps.push(step);
        }

        // Build single job
        let job = Job {
            name: Some(workflow_name.clone()),
            runs_on: self.runner_as_runs_on(),
            needs: Vec::new(),
            if_condition: None,
            strategy: None,
            environment: ir.pipeline.environment.clone().map(Environment::Name),
            env: IndexMap::new(),
            concurrency: None,
            continue_on_error: None,
            timeout_minutes: None,
            steps,
        };

        let mut jobs = IndexMap::new();
        jobs.insert(sanitize_job_id(&workflow_name), job);

        let workflow = Workflow {
            name: workflow_name,
            on: triggers,
            concurrency: Some(Concurrency {
                group: "${{ github.workflow }}-${{ github.head_ref || github.ref }}".to_string(),
                cancel_in_progress: Some(true),
            }),
            permissions: Some(permissions),
            env: IndexMap::new(),
            jobs,
        };

        Self::serialize_workflow(&workflow)
    }

    /// Emit an expanded mode GitHub Actions workflow.
    ///
    /// Expanded mode generates a multi-job workflow where each task becomes
    /// a separate job with dependencies managed by GitHub Actions (`needs:`).
    fn emit_expanded(&self, ir: &IntermediateRepresentation) -> EmitterResult<String> {
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

#[cfg(test)]
#[path = "emitter_tests.rs"]
mod tests;
