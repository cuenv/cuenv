//! Buildkite Pipeline Emitter
//!
//! Transforms cuenv IR into Buildkite pipeline YAML format.

use crate::schema::{AgentRules, BlockStep, CommandStep, CommandValue, DependsOn, Pipeline, Step};
use cuenv_ci::emitter::{Emitter, EmitterError, EmitterResult};
use cuenv_ci::ir::{IntermediateRepresentation, OutputType, Runtime, Task};
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
    pub fn with_emojis(mut self) -> Self {
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
        let mut steps = Vec::new();
        let mut approval_keys: HashMap<String, String> = HashMap::new();

        for task in &ir.tasks {
            // If task requires manual approval, insert a block step first
            if task.manual_approval {
                let approval_key = format!("{}-approval", task.id);
                let block_step = self.build_block_step(task, &approval_key);
                steps.push(Step::Block(block_step));
                approval_keys.insert(task.id.clone(), approval_key);
            }

            let command_step = self.build_command_step(task, ir, &approval_keys);
            steps.push(Step::Command(Box::new(command_step)));
        }

        Pipeline {
            steps,
            env: HashMap::new(),
        }
    }

    /// Build a command step from an IR task
    fn build_command_step(
        &self,
        task: &Task,
        ir: &IntermediateRepresentation,
        approval_keys: &HashMap<String, String>,
    ) -> CommandStep {
        let label = self.format_label(&task.id, task.deployment);

        // Build the command, wrapping with Nix setup if task has a runtime
        let base_command = format!("cuenv task {}", task.id);
        let command = if let Some(runtime_id) = &task.runtime {
            // Look up the runtime definition in the IR
            if let Some(runtime) = ir.runtimes.iter().find(|r| r.id == *runtime_id) {
                // Generate Nix-wrapped command with bootstrap
                let setup = Self::generate_nix_setup(runtime);
                Some(CommandValue::Single(format!(
                    "{}\nnix develop {}#{} --command {}",
                    setup, runtime.flake, runtime.output, base_command
                )))
            } else {
                // Runtime not found - fall back to plain command
                Some(CommandValue::Single(base_command))
            }
        } else {
            // No runtime - assume cuenv is available on the agent
            Some(CommandValue::Single(base_command))
        };

        // Environment variables are handled by cuenv task, but we still pass
        // through any orchestrator-level env vars that might be needed
        let env = task.env.clone();

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

        // Build dependencies
        let mut depends_on: Vec<DependsOn> = task
            .depends_on
            .iter()
            .map(|dep| {
                // Check if the dependency has an approval step
                if let Some(approval_key) = approval_keys.get(dep) {
                    DependsOn::Key(approval_key.clone())
                } else {
                    DependsOn::Key(dep.clone())
                }
            })
            .collect();

        // If this task has manual approval, depend on its own approval step
        if let Some(approval_key) = approval_keys.get(&task.id) {
            depends_on.push(DependsOn::Key(approval_key.clone()));
        }

        // Handle concurrency
        let (concurrency_group, concurrency) = if let Some(group) = &task.concurrency_group {
            (Some(group.clone()), Some(1))
        } else {
            (None, None)
        };

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

    /// Generate Nix bootstrap script for a runtime
    ///
    /// This script ensures Nix is installed and available. With proper Cachix
    /// configuration, subsequent `nix develop` commands will be fast cache hits.
    fn generate_nix_setup(_runtime: &Runtime) -> String {
        r"# Nix bootstrap (fast with cachix)
if ! command -v nix &> /dev/null; then
  curl -sSf -L https://install.determinate.systems/nix | sh -s -- install linux --no-confirm --init none
  . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh
fi"
        .to_string()
    }
}

impl Emitter for BuildkiteEmitter {
    fn emit(&self, ir: &IntermediateRepresentation) -> EmitterResult<String> {
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

    fn make_ir(tasks: Vec<Task>) -> IntermediateRepresentation {
        IntermediateRepresentation {
            version: "1.3".to_string(),
            pipeline: PipelineMetadata {
                name: "test-pipeline".to_string(),
                project_name: None,
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

    #[test]
    fn test_with_nix_runtime() {
        let emitter = BuildkiteEmitter::new();

        // Create a task that references a Nix runtime
        let mut task = make_task("build", &["cargo", "build"]);
        task.runtime = Some("nix-rust".to_string());

        // Create IR with runtime definition
        let mut ir = make_ir(vec![task]);
        ir.runtimes.push(Runtime {
            id: "nix-rust".to_string(),
            flake: "github:NixOS/nixpkgs/nixos-unstable".to_string(),
            output: "devShells.x86_64-linux.default".to_string(),
            system: "x86_64-linux".to_string(),
            digest: "sha256:abc123".to_string(),
            purity: PurityMode::Strict,
        });

        let yaml = emitter.emit(&ir).unwrap();

        // Should contain Nix bootstrap
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
