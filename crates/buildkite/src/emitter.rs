//! Buildkite Pipeline Emitter
//!
//! Transforms cuenv IR into Buildkite pipeline YAML format.

// Pipeline emission involves complex step generation with many options
#![allow(clippy::too_many_lines)]

use crate::schema::{AgentRules, BlockStep, CommandStep, CommandValue, DependsOn, Pipeline, Step};
use cuenv_ci::emitter::{Emitter, EmitterError, EmitterResult};
use cuenv_ci::ir::{BuildStage, IntermediateRepresentation, OutputType, Task};
use std::collections::HashMap;

/// Buildkite pipeline emitter
///
/// Transforms cuenv IR into Buildkite pipeline YAML that can be uploaded
/// via `buildkite-agent pipeline upload`.
///
/// # IR to Buildkite Mapping
///
/// | IR Field | Buildkite YAML |
/// |----------|----------------|
/// | `task.id` | `key` |
/// | `task.command` | `command` |
/// | `task.env` | `env` |
/// | `task.secrets` | `env` (variable references) |
/// | `task.depends_on` | `depends_on` |
/// | `task.resources.tags` | `agents: { queue: "tag" }` |
/// | `task.concurrency_group` | `concurrency_group` + `concurrency: 1` |
/// | `task.manual_approval` | `block` step before task |
/// | `task.outputs` (orchestrator) | `artifact_paths` |
#[derive(Debug, Clone, Default)]
pub struct BuildkiteEmitter {
    /// Add emoji prefixes to labels
    pub use_emojis: bool,
    /// Default queue for agents
    pub default_queue: Option<String>,
}

impl BuildkiteEmitter {
    /// Create a new Buildkite emitter
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable emoji prefixes in step labels
    #[must_use]
    pub const fn with_emojis(mut self) -> Self {
        self.use_emojis = true;
        self
    }

    /// Set a default queue for all steps
    #[must_use]
    pub fn with_default_queue(mut self, queue: impl Into<String>) -> Self {
        self.default_queue = Some(queue.into());
        self
    }

    /// Convert IR tasks to Buildkite pipeline
    fn build_pipeline(&self, ir: &IntermediateRepresentation) -> Pipeline {
        let mut steps: Vec<Step> = Vec::new();

        // Emit bootstrap phase tasks (e.g., install-nix)
        for task in ir.sorted_phase_tasks(BuildStage::Bootstrap) {
            steps.push(Step::Command(Box::new(self.phase_task_to_step(task))));
        }

        // Emit setup phase tasks (e.g., cachix, setup-cuenv, 1password)
        for task in ir.sorted_phase_tasks(BuildStage::Setup) {
            steps.push(Step::Command(Box::new(self.phase_task_to_step(task))));
        }

        // Collect all bootstrap + setup task IDs for dependencies
        let setup_keys: Vec<String> = ir
            .sorted_phase_tasks(BuildStage::Bootstrap)
            .iter()
            .chain(ir.sorted_phase_tasks(BuildStage::Setup).iter())
            .map(|t| t.id.clone())
            .collect();

        // Build approval keys map for tasks that need manual approval
        let approval_keys: HashMap<String, String> = ir
            .regular_tasks()
            .filter(|task| task.manual_approval)
            .map(|task| (task.id.clone(), format!("{}-approval", task.id)))
            .collect();

        // Build task steps: block steps for approvals, then command steps
        for task in ir.regular_tasks() {
            // Add block step if task requires manual approval
            if let Some(approval_key) = approval_keys.get(&task.id) {
                steps.push(Step::Block(self.build_block_step(task, approval_key)));
            }

            // Add command step for the task
            steps.push(Step::Command(Box::new(self.build_command_step(
                task,
                ir,
                &approval_keys,
                &setup_keys,
            ))));
        }

        Pipeline {
            steps,
            env: HashMap::new(),
        }
    }

    /// Convert a phase task (bootstrap/setup/success/failure) to a Buildkite command step
    fn phase_task_to_step(&self, task: &Task) -> CommandStep {
        let label = task.label.as_ref().map(|l| self.format_label(l, false));

        // Build command - use shell wrapper if needed
        let command = if task.shell {
            Some(CommandValue::Single(task.command.join(" ")))
        } else {
            Some(CommandValue::Array(task.command.clone()))
        };

        // Build dependencies
        let depends_on: Vec<DependsOn> = task
            .depends_on
            .iter()
            .map(|dep| DependsOn::Key(dep.clone()))
            .collect();

        CommandStep {
            label,
            key: Some(task.id.clone()),
            command,
            env: task
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            agents: None,
            artifact_paths: vec![],
            depends_on,
            concurrency_group: None,
            concurrency: None,
            retry: None,
            timeout_in_minutes: None,
            soft_fail: None,
        }
    }

    /// Build a command step from an IR task
    fn build_command_step(
        &self,
        task: &Task,
        ir: &IntermediateRepresentation,
        approval_keys: &HashMap<String, String>,
        setup_keys: &[String],
    ) -> CommandStep {
        let label = self.format_label(&task.id, task.deployment);

        // Build the command - Nix setup is handled by stage tasks
        let base_command = ir.pipeline.environment.as_ref().map_or_else(
            || format!("cuenv task {}", task.id),
            |env| format!("cuenv task {} -e {}", task.id, env),
        );

        // Wrap with nix develop if task has a runtime
        let command = if let Some(runtime_id) = &task.runtime {
            if let Some(runtime) = ir.runtimes.iter().find(|r| r.id == *runtime_id) {
                // Source nix profile and run in nix develop
                Some(CommandValue::Single(format!(
                    ". /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh && nix develop {}#{} --command {}",
                    runtime.flake, runtime.output, base_command
                )))
            } else {
                Some(CommandValue::Single(base_command))
            }
        } else {
            Some(CommandValue::Single(base_command))
        };

        // Environment variables - secrets are handled by stage tasks
        // Convert from BTreeMap to HashMap for Buildkite schema
        let env: HashMap<String, String> = task
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        // Build agent rules from resource tags
        let agents = task
            .resources
            .as_ref()
            .and_then(|r| AgentRules::from_tags(r.tags.clone()))
            .or_else(|| self.default_queue.as_ref().map(AgentRules::with_queue));

        // Build artifact paths from orchestrator outputs
        let artifact_paths: Vec<String> = task
            .outputs
            .iter()
            .filter(|o| o.output_type == OutputType::Orchestrator)
            .map(|o| o.path.clone())
            .collect();

        // Build dependencies: task depends on setup stages + explicit dependencies
        let mut depends_on: Vec<DependsOn> = Vec::new();

        // Add setup stage dependencies if task has a runtime or needs 1Password
        if task.runtime.is_some() || ir.pipeline.requires_onepassword {
            for setup_key in setup_keys {
                depends_on.push(DependsOn::Key(setup_key.clone()));
            }
        }

        // Add explicit task dependencies
        for dep in &task.depends_on {
            if let Some(approval_key) = approval_keys.get(dep) {
                depends_on.push(DependsOn::Key(approval_key.clone()));
            } else {
                depends_on.push(DependsOn::Key(dep.clone()));
            }
        }

        // If this task has manual approval, depend on its own approval step
        if let Some(approval_key) = approval_keys.get(&task.id) {
            depends_on.push(DependsOn::Key(approval_key.clone()));
        }

        // Handle concurrency
        let (concurrency_group, concurrency) = task
            .concurrency_group
            .as_ref()
            .map_or((None, None), |group| (Some(group.clone()), Some(1)));

        CommandStep {
            label: Some(label),
            key: Some(task.id.clone()),
            command,
            env,
            agents,
            artifact_paths,
            depends_on,
            concurrency_group,
            concurrency,
            retry: None,
            timeout_in_minutes: None,
            soft_fail: None,
        }
    }

    /// Build a block step for manual approval
    fn build_block_step(&self, task: &Task, approval_key: &str) -> BlockStep {
        let label = if self.use_emojis {
            format!(":hand: Approve {}", task.id)
        } else {
            format!("Approve {}", task.id)
        };

        let mut depends_on: Vec<DependsOn> = task
            .depends_on
            .iter()
            .map(|dep| DependsOn::Key(dep.clone()))
            .collect();

        // Block step inherits the task's dependencies
        if depends_on.is_empty() {
            depends_on = Vec::new();
        }

        BlockStep {
            block: label,
            key: Some(approval_key.to_string()),
            depends_on,
            prompt: Some(format!("Approve execution of {}", task.id)),
            fields: Vec::new(),
        }
    }

    /// Format a step label with optional emoji
    fn format_label(&self, task_id: &str, is_deployment: bool) -> String {
        if self.use_emojis {
            let emoji = if is_deployment { ":rocket:" } else { ":gear:" };
            format!("{emoji} {task_id}")
        } else {
            task_id.to_string()
        }
    }
}

impl Emitter for BuildkiteEmitter {
    /// Emit a thin mode Buildkite pipeline.
    ///
    /// Thin mode generates a bootstrap pipeline that:
    /// 1. Installs Nix
    /// 2. Builds cuenv
    /// 3. Calls `cuenv ci --pipeline <name>` for orchestration
    fn emit_thin(&self, ir: &IntermediateRepresentation) -> EmitterResult<String> {
        let pipeline_name = &ir.pipeline.name;

        // Build a bootstrap pipeline that delegates to cuenv
        let mut steps = Vec::new();

        // Bootstrap phase tasks (e.g., install-nix)
        for task in ir.sorted_phase_tasks(BuildStage::Bootstrap) {
            steps.push(Step::Command(Box::new(self.phase_task_to_step(task))));
        }

        // Setup phase tasks (e.g., cachix, setup-cuenv)
        for task in ir.sorted_phase_tasks(BuildStage::Setup) {
            steps.push(Step::Command(Box::new(self.phase_task_to_step(task))));
        }

        // Main execution step: cuenv ci --pipeline <name>
        let cuenv_command = format!("cuenv ci --pipeline {pipeline_name}");
        let main_step = CommandStep {
            label: Some(self.format_label(&format!("Run pipeline: {pipeline_name}"), false)),
            key: Some("cuenv-ci".to_string()),
            command: Some(CommandValue::Single(cuenv_command)),
            env: HashMap::new(),
            agents: self.default_queue.as_ref().map(AgentRules::with_queue),
            artifact_paths: vec![],
            depends_on: ir
                .sorted_phase_tasks(BuildStage::Setup)
                .last()
                .map(|t| vec![DependsOn::Key(t.id.clone())])
                .unwrap_or_default(),
            concurrency_group: None,
            concurrency: None,
            retry: None,
            timeout_in_minutes: None,
            soft_fail: None,
        };
        steps.push(Step::Command(Box::new(main_step)));

        let pipeline = Pipeline {
            steps,
            env: HashMap::new(),
        };

        serde_yaml::to_string(&pipeline).map_err(|e| EmitterError::Serialization(e.to_string()))
    }

    /// Emit an expanded mode Buildkite pipeline.
    ///
    /// Expanded mode generates a full pipeline where each task becomes a separate step
    /// with dependencies managed by Buildkite.
    fn emit_expanded(&self, ir: &IntermediateRepresentation) -> EmitterResult<String> {
        let pipeline = self.build_pipeline(ir);
        serde_yaml::to_string(&pipeline).map_err(|e| EmitterError::Serialization(e.to_string()))
    }

    fn format_name(&self) -> &'static str {
        "buildkite"
    }

    fn file_extension(&self) -> &'static str {
        "yml"
    }

    fn description(&self) -> &'static str {
        "Buildkite pipeline YAML emitter"
    }

    fn validate(&self, ir: &IntermediateRepresentation) -> EmitterResult<()> {
        // Validate that all task IDs are valid Buildkite keys
        for task in &ir.tasks {
            if task.id.contains(' ') {
                return Err(EmitterError::InvalidIR(format!(
                    "Task ID '{}' contains spaces, which are not allowed in Buildkite keys",
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

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_ci::ir::{
        CachePolicy, OutputDeclaration, PipelineMetadata, PurityMode, ResourceRequirements,
        Runtime, SecretConfig,
    };
    use cuenv_core::ci::PipelineMode;
    use std::collections::BTreeMap;

    /// Create an IR for testing expanded mode behavior.
    /// Uses PipelineMode::Expanded explicitly since tests check multi-job output.
    fn make_ir(tasks: Vec<Task>) -> IntermediateRepresentation {
        IntermediateRepresentation {
            version: "1.4".to_string(),
            pipeline: PipelineMetadata {
                name: "test-pipeline".to_string(),
                mode: PipelineMode::Expanded,
                environment: None,
                requires_onepassword: false,
                project_name: None,
                trigger: None,
                pipeline_tasks: vec![],
                pipeline_task_defs: vec![],
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

    #[test]
    fn test_simple_pipeline() {
        let emitter = BuildkiteEmitter::new();
        let ir = make_ir(vec![make_task("build", &["cargo", "build"])]);

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.contains("steps:"));
        assert!(yaml.contains("key: build"));
        // Commands are wrapped with cuenv task
        assert!(yaml.contains("cuenv task build"));
    }

    #[test]
    fn test_with_dependencies() {
        let emitter = BuildkiteEmitter::new();
        let mut test_task = make_task("test", &["cargo", "test"]);
        test_task.depends_on = vec!["build".to_string()];

        let ir = make_ir(vec![make_task("build", &["cargo", "build"]), test_task]);

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.contains("depends_on:"));
        assert!(yaml.contains("- build"));
    }

    #[test]
    fn test_with_manual_approval() {
        let emitter = BuildkiteEmitter::new().with_emojis();
        let mut deploy_task = make_task("deploy", &["./deploy.sh"]);
        deploy_task.manual_approval = true;
        deploy_task.deployment = true;

        let ir = make_ir(vec![deploy_task]);

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.contains("block:"));
        assert!(yaml.contains("Approve deploy"));
        assert!(yaml.contains("deploy-approval"));
    }

    #[test]
    fn test_with_concurrency_group() {
        let emitter = BuildkiteEmitter::new();
        let mut deploy_task = make_task("deploy", &["./deploy.sh"]);
        deploy_task.concurrency_group = Some("production".to_string());

        let ir = make_ir(vec![deploy_task]);

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.contains("concurrency_group: production"));
        assert!(yaml.contains("concurrency: 1"));
    }

    #[test]
    fn test_with_agent_queue() {
        let emitter = BuildkiteEmitter::new();
        let mut task = make_task("build", &["cargo", "build"]);
        task.resources = Some(ResourceRequirements {
            cpu: None,
            memory: None,
            tags: vec!["linux-x86".to_string()],
        });

        let ir = make_ir(vec![task]);

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.contains("agents:"));
        assert!(yaml.contains("queue: linux-x86"));
    }

    #[test]
    fn test_with_secrets() {
        let emitter = BuildkiteEmitter::new();
        let mut task = make_task("deploy", &["./deploy.sh"]);
        task.secrets.insert(
            "API_KEY".to_string(),
            SecretConfig {
                source: "BUILDKITE_SECRET_API_KEY".to_string(),
                cache_key: false,
            },
        );

        let ir = make_ir(vec![task]);

        let yaml = emitter.emit(&ir).unwrap();

        // Secrets are now handled by cuenv task internally, not mapped to env vars
        assert!(yaml.contains("cuenv task deploy"));
    }

    #[test]
    fn test_with_artifacts() {
        let emitter = BuildkiteEmitter::new();
        let mut task = make_task("build", &["cargo", "build"]);
        task.outputs = vec![OutputDeclaration {
            path: "target/release/binary".to_string(),
            output_type: OutputType::Orchestrator,
        }];

        let ir = make_ir(vec![task]);

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.contains("artifact_paths:"));
        assert!(yaml.contains("target/release/binary"));
    }

    #[test]
    fn test_default_queue() {
        let emitter = BuildkiteEmitter::new().with_default_queue("default");
        let ir = make_ir(vec![make_task("build", &["cargo", "build"])]);

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.contains("agents:"));
        assert!(yaml.contains("queue: default"));
    }

    #[test]
    fn test_emojis() {
        let emitter = BuildkiteEmitter::new().with_emojis();
        let ir = make_ir(vec![make_task("build", &["cargo", "build"])]);

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.contains(":gear:"));
    }

    #[test]
    fn test_validation_invalid_id() {
        let emitter = BuildkiteEmitter::new();
        let ir = make_ir(vec![make_task("invalid task", &["echo"])]);

        let result = emitter.validate(&ir);
        assert!(result.is_err());
    }

    #[test]
    fn test_validation_missing_dependency() {
        let emitter = BuildkiteEmitter::new();
        let mut task = make_task("test", &["cargo", "test"]);
        task.depends_on = vec!["nonexistent".to_string()];

        let ir = make_ir(vec![task]);

        let result = emitter.validate(&ir);
        assert!(result.is_err());
    }

    #[test]
    fn test_format_name() {
        let emitter = BuildkiteEmitter::new();
        assert_eq!(emitter.format_name(), "buildkite");
        assert_eq!(emitter.file_extension(), "yml");
    }

    /// Helper to create a phase task for testing
    fn make_phase_task(id: &str, command: &[&str], phase: BuildStage, priority: i32) -> Task {
        Task {
            id: id.to_string(),
            runtime: None,
            command: command.iter().map(|s| (*s).to_string()).collect(),
            shell: command.len() == 1, // Single command = shell mode
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
            phase: Some(phase),
            label: None,
            priority: Some(priority),
            contributor: None,
            condition: None,
            provider_hints: None,
        }
    }

    #[test]
    fn test_with_nix_runtime() {
        let emitter = BuildkiteEmitter::new();

        // Create a task that references a Nix runtime
        let mut task = make_task("build", &["cargo", "build"]);
        task.runtime = Some("nix-rust".to_string());

        // Create phase tasks that would be contributed by NixContributor
        let mut bootstrap_task = make_phase_task(
            "install-nix",
            &[
                "curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh -s -- install linux --no-confirm --init none",
            ],
            BuildStage::Bootstrap,
            0,
        );
        bootstrap_task.label = Some("Install Nix".to_string());
        bootstrap_task.contributor = Some("nix".to_string());

        let mut setup_task = make_phase_task(
            "setup-cuenv",
            &[
                ". /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh && nix build .#cuenv --accept-flake-config",
            ],
            BuildStage::Setup,
            10,
        );
        setup_task.label = Some("Setup cuenv".to_string());
        setup_task.contributor = Some("cuenv".to_string());
        setup_task.depends_on = vec!["install-nix".to_string()];

        // Create IR with runtime definition and phase tasks
        let mut ir = make_ir(vec![bootstrap_task, setup_task, task]);
        ir.runtimes.push(Runtime {
            id: "nix-rust".to_string(),
            flake: "github:NixOS/nixpkgs/nixos-unstable".to_string(),
            output: "devShells.x86_64-linux.default".to_string(),
            system: "x86_64-linux".to_string(),
            digest: "sha256:abc123".to_string(),
            purity: PurityMode::Strict,
        });

        let yaml = emitter.emit(&ir).unwrap();

        // Should contain Nix bootstrap from phase tasks
        assert!(yaml.contains("install.determinate.systems/nix"));
        assert!(yaml.contains("nix-daemon.sh"));

        // Should contain nix develop command with flake reference
        assert!(yaml.contains("nix develop"));
        assert!(yaml.contains("github:NixOS/nixpkgs/nixos-unstable"));
        assert!(yaml.contains("devShells.x86_64-linux.default"));

        // Should wrap cuenv task
        assert!(yaml.contains("cuenv task build"));
    }

    #[test]
    fn test_without_runtime_no_nix_setup() {
        let emitter = BuildkiteEmitter::new();

        // Task without runtime
        let task = make_task("build", &["cargo", "build"]);
        let ir = make_ir(vec![task]);

        let yaml = emitter.emit(&ir).unwrap();

        // Should NOT contain Nix bootstrap
        assert!(!yaml.contains("install.determinate.systems/nix"));
        assert!(!yaml.contains("nix develop"));

        // Should just have plain cuenv task
        assert!(yaml.contains("cuenv task build"));
    }
}
