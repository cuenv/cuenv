//! GitHub Actions Workflow Emitter
//!
//! Transforms cuenv IR into GitHub Actions workflow YAML files.

use crate::workflow::schema::{
    Concurrency, Environment, Job, PermissionLevel, Permissions, PullRequestTrigger, PushTrigger,
    ReleaseTrigger, RunsOn, ScheduleTrigger, Step, Workflow, WorkflowDispatchTrigger,
    WorkflowInput, WorkflowTriggers,
};
use cuenv_ci::emitter::{Emitter, EmitterError, EmitterResult};
use cuenv_ci::ir::{IntermediateRepresentation, OutputType, Task, TriggerCondition};
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
    pub fn from_config(config: &cuenv_core::ci::GitHubConfig) -> Self {
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
    pub fn with_nix(mut self) -> Self {
        self.use_nix = true;
        self
    }

    /// Disable Nix installation steps
    #[must_use]
    pub fn without_nix(mut self) -> Self {
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
    pub fn without_cuenv_build(mut self) -> Self {
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

        // Generate a single workflow with all tasks as jobs
        let workflow = self.build_workflow(ir);
        let filename = format!("{}.yml", sanitize_filename(&ir.pipeline.name));
        let yaml = Self::serialize_workflow(&workflow)?;
        workflows.insert(filename, yaml);

        Ok(workflows)
    }

    /// Build a workflow from the IR
    fn build_workflow(&self, ir: &IntermediateRepresentation) -> Workflow {
        let triggers = self.build_triggers(ir);
        let permissions = Self::build_permissions(ir);
        let jobs = self.build_jobs(ir);

        Workflow {
            name: ir.pipeline.name.clone(),
            on: triggers,
            concurrency: Some(Concurrency {
                group: "${{ github.workflow }}-${{ github.head_ref || github.ref }}".to_string(),
                cancel_in_progress: Some(true),
            }),
            permissions: Some(permissions),
            env: HashMap::new(),
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

        // Emit PR trigger if explicitly enabled or if we have branch conditions
        if !trigger.branches.is_empty() || trigger.pull_request == Some(true) {
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
            let job = self.build_job(task);
            jobs.insert(sanitize_job_id(&task.id), job);
        }

        jobs
    }

    /// Build a job from an IR task
    #[allow(clippy::too_many_lines)]
    fn build_job(&self, task: &Task) -> Job {
        let mut steps = Vec::new();

        // Checkout step
        steps.push(
            Step::uses("actions/checkout@v4")
                .with_name("Checkout")
                .with_input("fetch-depth", serde_yaml::Value::Number(2.into())),
        );

        // Nix installation
        if self.use_nix {
            steps.push(
                Step::uses("DeterminateSystems/nix-installer-action@v16")
                    .with_name("Install Nix")
                    .with_input(
                        "extra-conf",
                        serde_yaml::Value::String("accept-flake-config = true".to_string()),
                    ),
            );
        }

        // Cachix setup
        if self.use_cachix
            && let Some(cache_name) = &self.cachix_name
        {
            let mut cachix_step = Step::uses("cachix/cachix-action@v15")
                .with_name("Setup Cachix")
                .with_input("name", serde_yaml::Value::String(cache_name.clone()))
                .with_input(
                    "authToken",
                    serde_yaml::Value::String(format!(
                        "${{{{ secrets.{} }}}}",
                        self.cachix_auth_token_secret
                    )),
                );
            cachix_step.with_inputs.insert(
                "pushFilter".to_string(),
                serde_yaml::Value::String("(-source$|nixpkgs\\.tar\\.gz$)".to_string()),
            );
            steps.push(cachix_step);
        }

        // Build cuenv
        if self.build_cuenv && self.use_nix {
            steps.push(
                Step::run("nix build .#cuenv\necho \"$(pwd)/result/bin\" >> $GITHUB_PATH")
                    .with_name("Build cuenv"),
            );
        }

        // Run the task
        let mut task_step = Step::run(format!("cuenv task {}", task.id))
            .with_name(task.id.clone())
            .with_env("GITHUB_TOKEN", "${{ secrets.GITHUB_TOKEN }}");

        // Add task environment variables
        for (key, value) in &task.env {
            task_step.env.insert(key.clone(), value.clone());
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
            environment,
            env: HashMap::new(),
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
}

impl Emitter for GitHubActionsEmitter {
    fn emit(&self, ir: &IntermediateRepresentation) -> EmitterResult<String> {
        let workflow = self.build_workflow(ir);
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

/// Builder for creating workflows with release triggers
pub struct ReleaseWorkflowBuilder {
    emitter: GitHubActionsEmitter,
}

impl ReleaseWorkflowBuilder {
    /// Create a new release workflow builder
    #[must_use]
    pub fn new(emitter: GitHubActionsEmitter) -> Self {
        Self { emitter }
    }

    /// Build a release workflow from IR
    #[must_use]
    pub fn build(&self, ir: &IntermediateRepresentation) -> Workflow {
        let mut workflow = self.emitter.build_workflow(ir);

        // Override triggers for release workflows
        workflow.on = WorkflowTriggers {
            release: Some(ReleaseTrigger {
                types: vec!["published".to_string()],
            }),
            workflow_dispatch: Some(WorkflowDispatchTrigger {
                inputs: {
                    let mut inputs = HashMap::new();
                    inputs.insert(
                        "tag_name".to_string(),
                        WorkflowInput {
                            description: "Tag to release (e.g., 0.6.0)".to_string(),
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

        // Update permissions for releases
        workflow.permissions = Some(Permissions {
            contents: Some(PermissionLevel::Write),
            id_token: Some(PermissionLevel::Write),
            ..Default::default()
        });

        workflow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_ci::ir::{CachePolicy, PipelineMetadata, ResourceRequirements};

    fn make_ir(tasks: Vec<Task>) -> IntermediateRepresentation {
        IntermediateRepresentation {
            version: "1.3".to_string(),
            pipeline: PipelineMetadata {
                name: "test-pipeline".to_string(),
                trigger: None,
            },
            runtimes: vec![],
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
        let ir = make_ir(vec![make_task("build", &["cargo", "build"])]);

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.contains("DeterminateSystems/nix-installer-action"));
        assert!(yaml.contains("nix build .#cuenv"));
    }

    #[test]
    fn test_workflow_with_cachix() {
        let emitter = GitHubActionsEmitter::new()
            .with_nix()
            .with_cachix("my-cache");
        let ir = make_ir(vec![make_task("build", &["cargo", "build"])]);

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
}
