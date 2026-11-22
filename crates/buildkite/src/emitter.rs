//! Buildkite Pipeline Emitter
//!
//! Transforms cuenv IR into Buildkite pipeline YAML format.

use crate::schema::{AgentRules, BlockStep, CommandStep, CommandValue, DependsOn, Pipeline, Step};
use cuenv_ci::emitter::{Emitter, EmitterError, EmitterResult};
use cuenv_ci::ir::{BuildStage, IntermediateRepresentation, OutputType, Task};
use cuenv_core::manifest::{CacheProtocol, RemoteCache};
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
    remote_cache: RemoteCacheAccess,
}

/// Whether the provider event and configured ref allow requesting write credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildkiteCacheTrust {
    /// Only read credentials may be requested.
    Untrusted,
    /// A protected ref may request the write credential on the configured queue.
    Protected,
}

#[derive(Debug, Clone, Default)]
enum RemoteCacheAccess {
    #[default]
    Inherit,
    Disabled,
    Configured(Box<RemoteCache>, BuildkiteCacheTrust),
}

#[derive(Clone, Copy)]
enum LabelKind {
    Standard,
    Deployment,
}

struct StepDependencies<'step_dependencies> {
    approval_keys: &'step_dependencies HashMap<String, String>,
    setup_keys: &'step_dependencies [String],
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

    /// Configure remote cache credentials and storage for generated command steps.
    #[must_use]
    pub fn with_remote_cache(mut self, remote: RemoteCache, trust: BuildkiteCacheTrust) -> Self {
        self.remote_cache = RemoteCacheAccess::Configured(Box::new(remote), trust);
        self
    }

    /// Disable remote cache access when projects have conflicting settings.
    #[must_use]
    pub fn without_remote_cache(mut self) -> Self {
        self.remote_cache = RemoteCacheAccess::Disabled;
        self
    }

    fn apply_remote_cache(&self, step: &mut CommandStep) {
        let (remote, trust) = match &self.remote_cache {
            RemoteCacheAccess::Inherit => return,
            RemoteCacheAccess::Disabled => {
                disable_remote_cache(step, CacheDirectory::Isolated);
                return;
            }
            RemoteCacheAccess::Configured(remote, trust) => (remote, *trust),
        };
        let ci = remote.ci.as_ref();
        let directory = if ci.is_none_or(|ci| ci.isolated_cache_dir) {
            CacheDirectory::Isolated
        } else {
            CacheDirectory::Shared
        };
        if ci.is_some_and(|ci| ci.auth_mode == cuenv_core::manifest::RemoteCacheCiAuthMode::Ambient)
        {
            let can_upload = trust == BuildkiteCacheTrust::Protected && remote.upload;
            step.env
                .insert("CUENV_REMOTE_CACHE_UPLOAD".into(), can_upload.to_string());
            let directory_trust = if can_upload {
                BuildkiteCacheTrust::Protected
            } else {
                BuildkiteCacheTrust::Untrusted
            };
            set_cache_directory(step, directory, directory_trust);
            return;
        }
        let auth_env = remote.auth.as_ref().and_then(|auth| {
            auth.bearer_token_env
                .as_deref()
                .or_else(|| auth.header.as_ref().map(|header| header.value_env.as_str()))
        });
        let (Some(ci), Some(auth_env)) = (ci, auth_env.filter(|name| valid_secret_env(name)))
        else {
            disable_remote_cache(step, directory);
            return;
        };

        step.env.remove(auth_env);
        let write = if trust == BuildkiteCacheTrust::Protected && remote.upload {
            ci.write_queue
                .as_deref()
                .filter(|queue| !queue.is_empty())
                .zip(
                    ci.write_secret
                        .as_deref()
                        .filter(|secret| !secret.is_empty()),
                )
        } else {
            None
        };
        let read = ci
            .read_secret
            .as_deref()
            .filter(|secret| !secret.is_empty() && Some(*secret) != ci.write_secret.as_deref());
        let secret = if let Some((queue, secret)) = write {
            step.agents
                .get_or_insert_with(|| AgentRules::with_queue(queue))
                .queue = Some(queue.to_string());
            secret
        } else if let Some(secret) = read {
            secret
        } else {
            disable_remote_cache(step, directory);
            return;
        };

        // Buildkite injects the secret on job start according to the cluster's
        // access policy. The uploaded pipeline contains only the secret key.
        step.secrets
            .insert(auth_env.to_string(), secret.to_string());
        step.env
            .insert("CUENV_REMOTE_CACHE".into(), remote.endpoint.clone());
        step.env.insert(
            "CUENV_REMOTE_CACHE_PROTOCOL".into(),
            match remote.protocol {
                CacheProtocol::Grpc => "grpc",
                CacheProtocol::Http => "http",
            }
            .into(),
        );
        step.env.insert(
            "CUENV_REMOTE_CACHE_INSTANCE".into(),
            remote.instance.clone(),
        );
        step.env.insert(
            "CUENV_REMOTE_CACHE_UPLOAD".into(),
            write.is_some().to_string(),
        );
        let directory_trust = if write.is_some() {
            BuildkiteCacheTrust::Protected
        } else {
            BuildkiteCacheTrust::Untrusted
        };
        set_cache_directory(step, directory, directory_trust);
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
        let dependencies = StepDependencies {
            approval_keys: &approval_keys,
            setup_keys: &setup_keys,
        };

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
                &dependencies,
            ))));
        }

        Pipeline {
            steps,
            env: HashMap::new(),
        }
    }

    /// Convert a phase task (bootstrap/setup/success/failure) to a Buildkite command step
    fn phase_task_to_step(&self, task: &Task) -> CommandStep {
        let label = task
            .label
            .as_ref()
            .map(|label| self.format_label(label, LabelKind::Standard));

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
            secrets: HashMap::new(),
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
        dependencies: &StepDependencies<'_>,
    ) -> CommandStep {
        let kind = if task.deployment {
            LabelKind::Deployment
        } else {
            LabelKind::Standard
        };
        let label = self.format_label(&task.id, kind);

        // Build the command - Nix setup is handled by stage tasks
        let base_command = ir.pipeline.environment.as_ref().map_or_else(
            || format!("cuenv task {}", task.id),
            |env| format!("cuenv task {} -e {}", task.id, env),
        );

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
            for setup_key in dependencies.setup_keys {
                depends_on.push(DependsOn::Key(setup_key.clone()));
            }
        }

        // Add explicit task dependencies
        for dep in &task.depends_on {
            if let Some(approval_key) = dependencies.approval_keys.get(dep) {
                depends_on.push(DependsOn::Key(approval_key.clone()));
            } else {
                depends_on.push(DependsOn::Key(dep.clone()));
            }
        }

        // If this task has manual approval, depend on its own approval step
        if let Some(approval_key) = dependencies.approval_keys.get(&task.id) {
            depends_on.push(DependsOn::Key(approval_key.clone()));
        }

        // Handle concurrency
        let (concurrency_group, concurrency) = task
            .concurrency_group
            .as_ref()
            .map_or((None, None), |group| (Some(group.clone()), Some(1)));

        let mut step = CommandStep {
            label: Some(label),
            key: Some(task.id.clone()),
            command: None,
            env,
            secrets: HashMap::new(),
            agents,
            artifact_paths,
            depends_on,
            concurrency_group,
            concurrency,
            retry: None,
            timeout_in_minutes: None,
            soft_fail: None,
        };
        self.apply_remote_cache(&mut step);
        // Let cuenv acquire the runtime after withholding cache credentials.
        // A nix develop wrapper would run shell hooks before cuenv starts.
        let runtime = task
            .runtime
            .as_ref()
            .and_then(|id| ir.runtimes.iter().find(|runtime| runtime.id == *id));
        let command = match runtime.filter(|_| step.secrets.is_empty()) {
            Some(runtime) => format!(
                "if [ -f /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh ]; then . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh; else . \"$HOME/.nix-profile/etc/profile.d/nix.sh\"; fi && nix develop {}#{} --command {}",
                runtime.flake, runtime.output, base_command
            ),
            None => base_command,
        };
        step.command = Some(CommandValue::Single(command));
        step
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
    fn format_label(&self, task_id: &str, kind: LabelKind) -> String {
        if self.use_emojis {
            let emoji = match kind {
                LabelKind::Standard => ":gear:",
                LabelKind::Deployment => ":rocket:",
            };
            format!("{emoji} {task_id}")
        } else {
            task_id.to_string()
        }
    }
}

#[derive(Clone, Copy)]
enum CacheDirectory {
    Shared,
    Isolated,
}

fn disable_remote_cache(step: &mut CommandStep, directory: CacheDirectory) {
    step.env.insert("CUENV_REMOTE_CACHE".into(), String::new());
    step.env
        .insert("CUENV_REMOTE_CACHE_UPLOAD".into(), "false".into());
    set_cache_directory(step, directory, BuildkiteCacheTrust::Untrusted);
}

fn set_cache_directory(
    step: &mut CommandStep,
    directory: CacheDirectory,
    trust: BuildkiteCacheTrust,
) {
    if matches!(directory, CacheDirectory::Isolated) {
        let path = match trust {
            BuildkiteCacheTrust::Protected => "/tmp/cuenv-cache/trusted",
            BuildkiteCacheTrust::Untrusted => "/tmp/cuenv-cache/untrusted",
        };
        step.env.insert("CUENV_CACHE_DIR".into(), path.into());
    }
}

fn valid_secret_env(name: &str) -> bool {
    if matches!(
        name,
        "CUENV_REMOTE_CACHE"
            | "CUENV_REMOTE_CACHE_PROTOCOL"
            | "CUENV_REMOTE_CACHE_INSTANCE"
            | "CUENV_REMOTE_CACHE_UPLOAD"
            | "CUENV_CACHE_DIR"
    ) {
        return false;
    }
    if (name.starts_with("BK") || name.starts_with("BUILDKITE"))
        && !matches!(name, "BUILDKITE_API_TOKEN" | "BUILDKITE_ANALYTICS_TOKEN")
    {
        return false;
    }
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
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
        let mut main_step = CommandStep {
            label: Some(self.format_label(
                &format!("Run pipeline: {pipeline_name}"),
                LabelKind::Standard,
            )),
            key: Some("cuenv-ci".to_string()),
            command: Some(CommandValue::Single(cuenv_command)),
            env: HashMap::new(),
            secrets: HashMap::new(),
            agents: self.default_queue.as_ref().map(AgentRules::with_queue),
            artifact_paths: vec![],
            depends_on: ir
                .sorted_phase_tasks(BuildStage::Setup)
                .last()
                .map_or_default(|t| vec![DependsOn::Key(t.id.clone())]),
            concurrency_group: None,
            concurrency: None,
            retry: None,
            timeout_in_minutes: None,
            soft_fail: None,
        };
        self.apply_remote_cache(&mut main_step);
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
                project_path: None,
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
            github_task_filter: Vec::new(),
            github_permissions: std::collections::BTreeMap::new(),
            github_execution: None,
            provider_hints: None,
            external_dependencies: Vec::new(),
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
        let mut deployment = make_task("deploy", &["deploy"]);
        deployment.deployment = true;
        deployment.cache_policy = CachePolicy::Disabled;
        let ir = make_ir(vec![make_task("build", &["cargo", "build"]), deployment]);

        let yaml = emitter.emit(&ir).unwrap();

        assert!(yaml.contains(":gear: build"));
        assert!(yaml.contains(":rocket: deploy"));
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
    fn make_phase_task(id: &str, command: &[&str], phase: BuildStage) -> Task {
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
            priority: None,
            contributor: None,
            condition: None,
            github_task_filter: Vec::new(),
            github_permissions: std::collections::BTreeMap::new(),
            github_execution: None,
            provider_hints: None,
            external_dependencies: Vec::new(),
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
                "curl --proto '=https' --tlsv1.2 -sSf -L https://releases.nixos.org/nix/nix-2.31.2/install | sh -s -- --no-daemon --no-channel-add",
            ],
            BuildStage::Bootstrap,
        );
        bootstrap_task.priority = Some(0);
        bootstrap_task.label = Some("Install Nix".to_string());
        bootstrap_task.contributor = Some("nix".to_string());

        let mut setup_task = make_phase_task(
            "setup-cuenv",
            &[
                ". /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh && nix build .#cuenv --accept-flake-config",
            ],
            BuildStage::Setup,
        );
        setup_task.priority = Some(10);
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
        assert!(yaml.contains("releases.nixos.org/nix/nix-2.31.2/install"));
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
        assert!(!yaml.contains("releases.nixos.org/nix/nix-2.31.2/install"));
        assert!(!yaml.contains("nix develop"));

        // Should just have plain cuenv task
        assert!(yaml.contains("cuenv task build"));
    }

    #[test]
    fn remote_cache_preserves_agent_tags_and_gates_native_secrets() -> EmitterResult<()> {
        use cuenv_core::manifest::{CacheAuth, RemoteCacheCi};

        let remote = RemoteCache {
            endpoint: "grpcs://cache.example.com:443".into(),
            protocol: CacheProtocol::Grpc,
            instance: "main".into(),
            upload: true,
            auth: Some(CacheAuth {
                bearer_token_env: Some("CUENV_CACHE_TOKEN".into()),
                header: None,
                credential_helper: None,
            }),
            ci: Some(RemoteCacheCi {
                read_secret: Some("CACHE_RO_TOKEN".into()),
                write_secret: Some("CACHE_RW_TOKEN".into()),
                write_environment: None,
                write_queue: Some("cache-write".into()),
                isolated_cache_dir: true,
                ..Default::default()
            }),
        };

        let mut task = make_task("build", &["cargo", "build"]);
        task.resources = Some(ResourceRequirements {
            tags: vec!["queue=linux".into(), "arch=arm64".into()],
            ..Default::default()
        });
        task.runtime = Some("nix-rust".to_string());
        let mut ir = make_ir(vec![task]);
        ir.runtimes.push(Runtime {
            id: "nix-rust".to_string(),
            flake: ".".to_string(),
            output: "devShells.x86_64-linux.default".to_string(),
            system: "x86_64-linux".to_string(),
            digest: "sha256:abc123".to_string(),
            purity: PurityMode::Strict,
        });
        let trusted = BuildkiteEmitter::new()
            .with_remote_cache(remote.clone(), BuildkiteCacheTrust::Protected);
        let untrusted =
            BuildkiteEmitter::new().with_remote_cache(remote, BuildkiteCacheTrust::Untrusted);

        let protected_yaml = trusted.emit_expanded(&ir)?;
        assert!(protected_yaml.contains("command: cuenv task build"));
        assert!(!protected_yaml.contains("nix develop"));
        assert!(protected_yaml.contains("queue: cache-write"));
        assert!(protected_yaml.contains("arch: arm64"));
        assert!(protected_yaml.contains("CUENV_CACHE_TOKEN: CACHE_RW_TOKEN"));
        assert!(!protected_yaml.contains("CACHE_RO_TOKEN"));
        assert!(protected_yaml.contains("CUENV_REMOTE_CACHE_UPLOAD: 'true'"));
        assert!(protected_yaml.contains("/tmp/cuenv-cache/trusted"));

        let untrusted_yaml = untrusted.emit_expanded(&ir)?;
        assert!(!untrusted_yaml.contains("queue: cache-write"));
        assert!(untrusted_yaml.contains("queue: linux"));
        assert!(untrusted_yaml.contains("arch: arm64"));
        assert!(untrusted_yaml.contains("CUENV_CACHE_TOKEN: CACHE_RO_TOKEN"));
        assert!(!untrusted_yaml.contains("CACHE_RW_TOKEN"));
        assert!(untrusted_yaml.contains("CUENV_REMOTE_CACHE_UPLOAD: 'false'"));
        assert!(untrusted_yaml.contains("/tmp/cuenv-cache/untrusted"));

        let thin_yaml = trusted.emit_thin(&ir)?;
        assert!(thin_yaml.contains("cuenv ci --pipeline test-pipeline"));
        assert!(thin_yaml.contains("CUENV_CACHE_TOKEN: CACHE_RW_TOKEN"));
        assert!(thin_yaml.contains("queue: cache-write"));
        Ok(())
    }

    #[test]
    fn remote_cache_ambient_mode_preserves_isolation_and_gates_uploads() -> EmitterResult<()> {
        use cuenv_core::manifest::{CacheAuth, RemoteCacheCi, RemoteCacheCiAuthMode};

        let remote = RemoteCache {
            endpoint: "grpcs://cache.namespace.so:443".into(),
            protocol: CacheProtocol::Grpc,
            instance: String::new(),
            upload: true,
            auth: Some(CacheAuth {
                bearer_token_env: None,
                header: None,
                credential_helper: Some("bazel-credential-nsc".into()),
            }),
            ci: Some(RemoteCacheCi {
                auth_mode: RemoteCacheCiAuthMode::Ambient,
                isolated_cache_dir: true,
                ..Default::default()
            }),
        };

        let ir = make_ir(vec![make_task("build", &["cargo", "build"])]);
        let trusted = BuildkiteEmitter::new()
            .with_remote_cache(remote.clone(), BuildkiteCacheTrust::Protected);
        let untrusted =
            BuildkiteEmitter::new().with_remote_cache(remote, BuildkiteCacheTrust::Untrusted);

        let protected_yaml = trusted.emit_thin(&ir)?;
        assert!(!protected_yaml.contains("CUENV_REMOTE_CACHE:"));
        assert!(protected_yaml.contains("CUENV_REMOTE_CACHE_UPLOAD: 'true'"));
        assert!(protected_yaml.contains("/tmp/cuenv-cache/trusted"));

        let untrusted_yaml = untrusted.emit_thin(&ir)?;
        assert!(!untrusted_yaml.contains("CUENV_REMOTE_CACHE:"));
        assert!(untrusted_yaml.contains("CUENV_REMOTE_CACHE_UPLOAD: 'false'"));
        assert!(untrusted_yaml.contains("/tmp/cuenv-cache/untrusted"));
        Ok(())
    }
}
