//! Compiler from cuenv task definitions to IR v1.4
//!
//! Transforms a cuenv `Project` with tasks into an intermediate representation
//! suitable for emitting orchestrator-native CI configurations.
//!
//! ## Stage Contributors
//!
//! The compiler applies stage contributors (Nix, 1Password, Cachix) during
//! compilation to inject setup/teardown tasks into the IR stages.

// IR compilation involves complex transformations with many fields
#![allow(clippy::too_many_lines)]

pub mod digest;

use crate::flake::{FlakeLockAnalyzer, FlakeLockError, PurityAnalysis};
use crate::ir::{
    CachePolicy, IntermediateRepresentation, IrValidator, ManualTriggerConfig, OutputDeclaration,
    OutputType, PurityMode, Runtime, SecretConfig, Task as IrTask, TriggerCondition,
    WorkflowDispatchInputDef,
};
use crate::stages;
use cuenv_core::ci::{CI, ManualTrigger, Pipeline, PipelineTask};
use cuenv_core::manifest::Project;
use cuenv_core::tasks::{Task, TaskDefinition, TaskGroup};
use digest::DigestBuilder;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

/// Compiler errors
#[derive(Debug, Error)]
pub enum CompilerError {
    #[error("Task graph validation failed: {0}")]
    ValidationFailed(String),

    #[error("Task '{0}' uses shell script but IR requires command array")]
    ShellScriptNotSupported(String),

    #[error("Invalid task structure: {0}")]
    InvalidTaskStructure(String),

    #[error("Flake lock error: {0}")]
    FlakeLock(#[from] FlakeLockError),
}

/// Compiler for transforming cuenv tasks to IR
pub struct Compiler {
    /// Project being compiled
    project: Project,

    /// Compiler options
    options: CompilerOptions,
}

/// Compiler configuration options
#[derive(Debug, Clone, Default)]
pub struct CompilerOptions {
    /// Default purity mode for runtimes
    pub purity_mode: PurityMode,

    /// Whether to validate inputs exist at compile time
    pub validate_inputs: bool,

    /// Default cache policy for tasks
    pub default_cache_policy: CachePolicy,

    /// Path to flake.lock file (optional, auto-detected if not set)
    pub flake_lock_path: Option<PathBuf>,

    /// Project root directory (for locating flake.lock)
    pub project_root: Option<PathBuf>,

    /// Manual overrides for input digests (for Override mode)
    /// Maps input name to override digest value
    pub input_overrides: HashMap<String, String>,

    /// Pipeline being compiled (for environment-aware compilation)
    ///
    /// When set, the compiler will set `ir.pipeline.environment` from
    /// the pipeline's environment, enabling contributors to self-detect
    /// their requirements.
    pub pipeline: Option<Pipeline>,
}

impl Compiler {
    /// Create a new compiler for the given project
    #[must_use]
    pub fn new(project: Project) -> Self {
        Self {
            project,
            options: CompilerOptions::default(),
        }
    }

    /// Create a compiler with custom options
    #[must_use]
    pub const fn with_options(project: Project, options: CompilerOptions) -> Self {
        Self { project, options }
    }

    /// Analyze flake.lock for purity and compute runtime digest
    ///
    /// If a flake.lock file is found, analyzes it for unlocked inputs
    /// and computes a deterministic digest based on the locked content.
    ///
    /// # Returns
    /// - `Some(Ok((digest, purity)))` if analysis succeeded
    /// - `Some(Err(e))` if analysis failed
    /// - `None` if no flake.lock was found (not a flake-based project)
    #[must_use]
    pub fn analyze_flake_purity(&self) -> Option<Result<(String, PurityMode), CompilerError>> {
        let lock_path = self.resolve_flake_lock_path();

        if !lock_path.exists() {
            return None;
        }

        Some(self.perform_flake_analysis(&lock_path))
    }

    /// Resolve the path to flake.lock
    fn resolve_flake_lock_path(&self) -> PathBuf {
        // Use explicit path if provided
        if let Some(path) = &self.options.flake_lock_path {
            return path.clone();
        }

        // Otherwise, look in project root
        if let Some(root) = &self.options.project_root {
            return root.join("flake.lock");
        }

        // Default: current directory
        PathBuf::from("flake.lock")
    }

    /// Perform flake purity analysis and apply purity mode
    fn perform_flake_analysis(
        &self,
        lock_path: &Path,
    ) -> Result<(String, PurityMode), CompilerError> {
        let analyzer = FlakeLockAnalyzer::from_path(lock_path)?;
        let analysis = analyzer.analyze();

        self.apply_purity_mode(&analysis)
    }

    /// Apply purity mode enforcement based on analysis results
    ///
    /// - **Strict**: Reject unlocked flakes with an error
    /// - **Warning**: Log warnings and inject UUID into digest (non-deterministic)
    /// - **Override**: Apply manual input overrides for deterministic builds
    fn apply_purity_mode(
        &self,
        analysis: &PurityAnalysis,
    ) -> Result<(String, PurityMode), CompilerError> {
        match self.options.purity_mode {
            PurityMode::Strict => {
                if !analysis.is_pure {
                    let inputs: Vec<String> = analysis
                        .unlocked_inputs
                        .iter()
                        .map(|u| format!("{}: {}", u.name, u.reason))
                        .collect();
                    return Err(CompilerError::FlakeLock(FlakeLockError::strict_violation(
                        inputs,
                    )));
                }
                Ok((analysis.locked_digest.clone(), PurityMode::Strict))
            }

            PurityMode::Warning => {
                if analysis.is_pure {
                    Ok((analysis.locked_digest.clone(), PurityMode::Warning))
                } else {
                    // Log warnings for each unlocked input
                    for input in &analysis.unlocked_inputs {
                        tracing::warn!(
                            input = %input.name,
                            reason = %input.reason,
                            "Unlocked flake input detected - cache key will be non-deterministic"
                        );
                    }

                    // Inject UUID v4 into digest to force cache miss
                    let uuid = Uuid::new_v4().to_string();
                    let mut digest_builder = DigestBuilder::new();
                    digest_builder.add_inputs(std::slice::from_ref(&analysis.locked_digest));
                    digest_builder.add_impurity_uuid(&uuid);

                    Ok((digest_builder.finalize(), PurityMode::Warning))
                }
            }

            PurityMode::Override => {
                // In override mode, apply manual input overrides
                let mut effective_digest = analysis.locked_digest.clone();

                if !self.options.input_overrides.is_empty() {
                    let mut digest_builder = DigestBuilder::new();
                    digest_builder.add_inputs(&[effective_digest]);

                    // Add overrides to digest in deterministic order
                    let mut sorted_overrides: Vec<_> =
                        self.options.input_overrides.iter().collect();
                    sorted_overrides.sort_by_key(|(k, _)| *k);

                    for (key, value) in sorted_overrides {
                        digest_builder.add_inputs(&[format!("override:{key}={value}")]);
                    }

                    effective_digest = digest_builder.finalize();
                }

                Ok((effective_digest, PurityMode::Override))
            }
        }
    }

    /// Compute a runtime configuration from the flake analysis
    ///
    /// This method creates a `Runtime` IR type with the computed digest
    /// based on flake purity analysis.
    ///
    /// # Errors
    ///
    /// Returns `CompilerError` if flake purity analysis fails.
    pub fn compute_runtime(
        &self,
        id: impl Into<String>,
        flake_ref: impl Into<String>,
        output: impl Into<String>,
        system: impl Into<String>,
    ) -> Result<Runtime, CompilerError> {
        let (digest, purity) = match self.analyze_flake_purity() {
            Some(result) => result?,
            None => {
                // No flake.lock found - use placeholder digest
                // This handles non-flake projects gracefully
                ("sha256:no-flake-lock".to_string(), self.options.purity_mode)
            }
        };

        Ok(Runtime {
            id: id.into(),
            flake: flake_ref.into(),
            output: output.into(),
            system: system.into(),
            digest,
            purity,
        })
    }

    /// Compile project tasks to IR
    ///
    /// # Errors
    ///
    /// Returns `CompilerError` if task compilation fails.
    pub fn compile(&self) -> Result<IntermediateRepresentation, CompilerError> {
        let mut ir = IntermediateRepresentation::new(&self.project.name);

        // Set pipeline context from options (enables environment-aware contributors)
        if let Some(ref pipeline) = self.options.pipeline {
            ir.pipeline.environment.clone_from(&pipeline.environment);
            ir.pipeline.pipeline_tasks = pipeline
                .tasks
                .iter()
                .map(PipelineTask::task_name)
                .map(String::from)
                .collect();
        }

        // Set up trigger conditions from CI configuration
        if let Some(ci_config) = &self.project.ci
            && let Some(first_pipeline) = ci_config.pipelines.first()
        {
            ir.pipeline.trigger = Some(self.build_trigger_condition(first_pipeline, ci_config));
        }

        // Compile tasks
        self.compile_tasks(&self.project.tasks, &mut ir)?;

        // Apply stage contributors with fixed-point iteration
        // Contributors self-detect their requirements and report modifications.
        // Loop continues until no contributor reports changes (stable state).
        let contributors = stages::default_contributors();
        loop {
            let mut any_modified = false;
            for contributor in &contributors {
                if contributor.is_active(&ir, &self.project) {
                    let (contributions, modified) = contributor.contribute(&ir, &self.project);
                    for (stage, task) in contributions {
                        ir.stages.add(stage, task);
                    }
                    any_modified |= modified;
                }
            }
            ir.stages.sort_by_priority();
            if !any_modified {
                break;
            }
        }

        // Validate the IR
        let validator = IrValidator::new(&ir);
        validator.validate().map_err(|errors| {
            let error_messages: Vec<String> = errors
                .iter()
                .map(std::string::ToString::to_string)
                .collect();
            CompilerError::ValidationFailed(error_messages.join(", "))
        })?;

        Ok(ir)
    }

    /// Build trigger condition for a pipeline from its configuration
    fn build_trigger_condition(&self, pipeline: &Pipeline, ci_config: &CI) -> TriggerCondition {
        let when = pipeline.when.as_ref();

        // Extract branch patterns
        let branches = when
            .and_then(|w| w.branch.as_ref())
            .map(cuenv_core::ci::StringOrVec::to_vec)
            .unwrap_or_default();

        // Extract pull_request setting
        let pull_request = when.and_then(|w| w.pull_request);

        // Extract scheduled cron expressions
        let scheduled = when
            .and_then(|w| w.scheduled.as_ref())
            .map(cuenv_core::ci::StringOrVec::to_vec)
            .unwrap_or_default();

        // Extract release types
        let release = when.and_then(|w| w.release.clone()).unwrap_or_default();

        // Build manual trigger config
        let manual = when.and_then(|w| w.manual.as_ref()).map(|m| match m {
            ManualTrigger::Enabled(enabled) => ManualTriggerConfig {
                enabled: *enabled,
                inputs: HashMap::new(),
            },
            ManualTrigger::WithInputs(inputs) => ManualTriggerConfig {
                enabled: true,
                inputs: inputs
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            WorkflowDispatchInputDef {
                                description: v.description.clone(),
                                required: v.required.unwrap_or(false),
                                default: v.default.clone(),
                                input_type: v.input_type.clone(),
                                options: v.options.clone().unwrap_or_default(),
                            },
                        )
                    })
                    .collect(),
            },
        });

        // Determine whether to derive paths from task inputs
        let should_derive_paths = pipeline.derive_paths.unwrap_or_else(|| {
            // Default: derive paths if we have branch/PR triggers (not scheduled-only)
            !branches.is_empty() || pull_request.is_some()
        });

        // Derive paths from task inputs
        let paths = if should_derive_paths {
            self.derive_trigger_paths(pipeline)
        } else {
            Vec::new()
        };

        // Get paths_ignore from provider config
        let paths_ignore = ci_config
            .github_config_for_pipeline(&pipeline.name)
            .paths_ignore
            .unwrap_or_default();

        TriggerCondition {
            branches,
            pull_request,
            scheduled,
            release,
            manual,
            paths,
            paths_ignore,
        }
    }

    /// Derive trigger paths from task inputs
    fn derive_trigger_paths(&self, pipeline: &Pipeline) -> Vec<String> {
        let mut paths = HashSet::new();

        // Collect inputs from all pipeline tasks (including transitive deps)
        for task in &pipeline.tasks {
            self.collect_task_inputs(task.task_name(), &mut paths);
        }

        // Add implicit CUE inputs (changes here should always trigger)
        paths.insert("env.cue".to_string());
        paths.insert("schema/**".to_string());
        paths.insert("cue.mod/**".to_string());

        // Sort for deterministic output
        let mut result: Vec<_> = paths.into_iter().collect();
        result.sort();
        result
    }

    /// Recursively collect task inputs including dependencies
    fn collect_task_inputs(&self, task_name: &str, paths: &mut HashSet<String>) {
        if let Some(task) = self.find_task(task_name) {
            // Add direct inputs
            for input in task.iter_path_inputs() {
                paths.insert(input.clone());
            }
            // Recurse into dependencies
            for dep in &task.depends_on {
                self.collect_task_inputs(dep, paths);
            }
        }
    }

    /// Find a task by name (handles dotted paths for nested tasks)
    fn find_task(&self, name: &str) -> Option<&Task> {
        let parts: Vec<&str> = name.split('.').collect();
        let mut current_tasks = &self.project.tasks;

        for (i, part) in parts.iter().enumerate() {
            match current_tasks.get(*part) {
                Some(TaskDefinition::Single(task)) if i == parts.len() - 1 => {
                    return Some(task);
                }
                Some(TaskDefinition::Group(TaskGroup::Parallel(parallel))) => {
                    current_tasks = &parallel.tasks;
                }
                _ => return None,
            }
        }
        None
    }

    /// Compile task definitions into IR tasks
    fn compile_tasks(
        &self,
        tasks: &HashMap<String, TaskDefinition>,
        ir: &mut IntermediateRepresentation,
    ) -> Result<(), CompilerError> {
        // Sort keys for deterministic output
        let mut sorted_keys: Vec<_> = tasks.keys().collect();
        sorted_keys.sort();
        for name in sorted_keys {
            let task_def = &tasks[name];
            self.compile_task_definition(name, task_def, ir)?;
        }
        Ok(())
    }

    /// Compile a single task definition (handles groups and single tasks)
    fn compile_task_definition(
        &self,
        name: &str,
        task_def: &TaskDefinition,
        ir: &mut IntermediateRepresentation,
    ) -> Result<(), CompilerError> {
        match task_def {
            TaskDefinition::Single(task) => {
                let ir_task = self.compile_single_task(name, task)?;
                ir.tasks.push(ir_task);
            }
            TaskDefinition::Group(group) => {
                self.compile_task_group(name, group, ir)?;
            }
        }
        Ok(())
    }

    /// Compile a task group (sequential or parallel)
    fn compile_task_group(
        &self,
        prefix: &str,
        group: &TaskGroup,
        ir: &mut IntermediateRepresentation,
    ) -> Result<(), CompilerError> {
        match group {
            TaskGroup::Sequential(tasks) => {
                for (idx, task_def) in tasks.iter().enumerate() {
                    let task_name = format!("{prefix}.{idx}");
                    self.compile_task_definition(&task_name, task_def, ir)?;
                }
            }
            TaskGroup::Parallel(parallel) => {
                // Sort keys for deterministic output
                let mut sorted_keys: Vec<_> = parallel.tasks.keys().collect();
                sorted_keys.sort();
                for name in sorted_keys {
                    let task_def = &parallel.tasks[name];
                    let task_name = format!("{prefix}.{name}");
                    self.compile_task_definition(&task_name, task_def, ir)?;
                }
            }
        }
        Ok(())
    }

    /// Compile a single task to IR format
    fn compile_single_task(&self, id: &str, task: &Task) -> Result<IrTask, CompilerError> {
        // Convert command and args to array format
        let command = if !task.command.is_empty() {
            let mut cmd = vec![task.command.clone()];
            cmd.extend(task.args.clone());
            cmd
        } else if let Some(script) = &task.script {
            // For scripts, we need to use shell mode
            // Note: This is a simplified approach; full implementation would
            // need to handle shebang parsing for polyglot scripts
            vec!["/bin/sh".to_string(), "-c".to_string(), script.clone()]
        } else {
            return Err(CompilerError::InvalidTaskStructure(format!(
                "Task '{id}' has neither command nor script"
            )));
        };

        // Determine shell mode
        let shell = task.shell.is_some() || task.script.is_some();

        // Convert environment variables (filter out complex JSON values)
        let env: HashMap<String, String> = task
            .env
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect();

        // Extract secrets (simplified - would integrate with secret resolver)
        let secrets: HashMap<String, SecretConfig> = HashMap::new();

        // Convert inputs (path globs only for now)
        let inputs: Vec<String> = task.iter_path_inputs().cloned().collect();

        // Convert outputs
        let outputs: Vec<OutputDeclaration> = task
            .outputs
            .iter()
            .map(|path| OutputDeclaration {
                path: path.clone(),
                output_type: OutputType::Cas, // Default to CAS
            })
            .collect();

        // Determine cache policy
        let cache_policy = if task.labels.contains(&"deployment".to_string()) {
            CachePolicy::Disabled
        } else {
            self.options.default_cache_policy
        };

        // Determine if this is a deployment task
        let deployment = task.labels.contains(&"deployment".to_string());

        Ok(IrTask {
            id: id.to_string(),
            runtime: None, // Would be set based on Nix flake configuration
            command,
            shell,
            env,
            secrets,
            resources: None, // Would extract from task metadata if available
            concurrency_group: None,
            inputs,
            outputs,
            depends_on: task.depends_on.clone(),
            cache_policy,
            deployment,
            manual_approval: false, // Would come from task metadata
            matrix: None,
            artifact_downloads: vec![],
            params: HashMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::tasks::Task;

    #[test]
    fn test_compile_simple_task() {
        let mut project = Project::new("test-project");
        project.tasks.insert(
            "build".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo".to_string(),
                args: vec!["build".to_string()],
                inputs: vec![cuenv_core::tasks::Input::Path("src/**/*.rs".to_string())],
                outputs: vec!["target/debug/binary".to_string()],
                ..Default::default()
            })),
        );

        let compiler = Compiler::new(project);
        let ir = compiler.compile().unwrap();

        assert_eq!(ir.version, "1.4");
        assert_eq!(ir.pipeline.name, "test-project");
        assert_eq!(ir.tasks.len(), 1);
        assert_eq!(ir.tasks[0].id, "build");
        assert_eq!(ir.tasks[0].command, vec!["cargo", "build"]);
        assert_eq!(ir.tasks[0].inputs, vec!["src/**/*.rs"]);
    }

    #[test]
    fn test_compile_task_with_dependencies() {
        let mut project = Project::new("test-project");

        project.tasks.insert(
            "test".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo".to_string(),
                args: vec!["test".to_string()],
                depends_on: vec!["build".to_string()],
                ..Default::default()
            })),
        );

        project.tasks.insert(
            "build".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo".to_string(),
                args: vec!["build".to_string()],
                ..Default::default()
            })),
        );

        let compiler = Compiler::new(project);
        let ir = compiler.compile().unwrap();

        assert_eq!(ir.tasks.len(), 2);

        let test_task = ir.tasks.iter().find(|t| t.id == "test").unwrap();
        assert_eq!(test_task.depends_on, vec!["build"]);
    }

    #[test]
    fn test_compile_deployment_task() {
        let mut project = Project::new("test-project");

        project.tasks.insert(
            "deploy".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "kubectl".to_string(),
                args: vec!["apply".to_string()],
                labels: vec!["deployment".to_string()],
                ..Default::default()
            })),
        );

        let compiler = Compiler::new(project);
        let ir = compiler.compile().unwrap();

        assert_eq!(ir.tasks.len(), 1);
        assert!(ir.tasks[0].deployment);
        assert_eq!(ir.tasks[0].cache_policy, CachePolicy::Disabled);
    }

    #[test]
    fn test_compile_script_task() {
        let mut project = Project::new("test-project");

        project.tasks.insert(
            "script-task".to_string(),
            TaskDefinition::Single(Box::new(Task {
                script: Some("echo 'Running script'\nls -la".to_string()),
                ..Default::default()
            })),
        );

        let compiler = Compiler::new(project);
        let ir = compiler.compile().unwrap();

        assert_eq!(ir.tasks.len(), 1);
        assert!(ir.tasks[0].shell);
        assert_eq!(ir.tasks[0].command[0], "/bin/sh");
        assert_eq!(ir.tasks[0].command[1], "-c");
    }

    #[test]
    fn test_purity_analysis_pure_flake() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "locked": {
                        "type": "github",
                        "owner": "NixOS",
                        "repo": "nixpkgs",
                        "rev": "abc123",
                        "narHash": "sha256-xxxxxxxxxxxxx"
                    }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(json.as_bytes()).unwrap();

        let project = Project::new("test-project");
        let options = CompilerOptions {
            purity_mode: PurityMode::Strict,
            flake_lock_path: Some(temp_file.path().to_path_buf()),
            ..Default::default()
        };

        let compiler = Compiler::with_options(project, options);
        let result = compiler.analyze_flake_purity();

        assert!(result.is_some());
        let (digest, purity) = result.unwrap().unwrap();
        assert!(digest.starts_with("sha256:"));
        assert_eq!(purity, PurityMode::Strict);
    }

    #[test]
    fn test_purity_strict_mode_rejects_unlocked() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs" }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(json.as_bytes()).unwrap();

        let project = Project::new("test-project");
        let options = CompilerOptions {
            purity_mode: PurityMode::Strict,
            flake_lock_path: Some(temp_file.path().to_path_buf()),
            ..Default::default()
        };

        let compiler = Compiler::with_options(project, options);
        let result = compiler.analyze_flake_purity();

        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn test_purity_warning_mode_injects_uuid() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs" }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(json.as_bytes()).unwrap();

        let project = Project::new("test-project");
        let options = CompilerOptions {
            purity_mode: PurityMode::Warning,
            flake_lock_path: Some(temp_file.path().to_path_buf()),
            ..Default::default()
        };

        let compiler = Compiler::with_options(project.clone(), options.clone());
        let result1 = compiler.analyze_flake_purity().unwrap().unwrap();

        let compiler2 = Compiler::with_options(project, options);
        let result2 = compiler2.analyze_flake_purity().unwrap().unwrap();

        // Each compile should produce different digests due to UUID injection
        assert_ne!(result1.0, result2.0);
        assert_eq!(result1.1, PurityMode::Warning);
    }

    #[test]
    fn test_purity_override_mode_uses_overrides() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "locked": {
                        "type": "github",
                        "narHash": "sha256-base"
                    }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(json.as_bytes()).unwrap();

        let mut input_overrides = HashMap::new();
        input_overrides.insert("nixpkgs".to_string(), "sha256-custom".to_string());

        let project = Project::new("test-project");
        let options = CompilerOptions {
            purity_mode: PurityMode::Override,
            flake_lock_path: Some(temp_file.path().to_path_buf()),
            input_overrides,
            ..Default::default()
        };

        let compiler = Compiler::with_options(project.clone(), options.clone());
        let result1 = compiler.analyze_flake_purity().unwrap().unwrap();

        // Same compiler, same overrides = deterministic digest
        let compiler2 = Compiler::with_options(project, options);
        let result2 = compiler2.analyze_flake_purity().unwrap().unwrap();

        assert_eq!(result1.0, result2.0);
        assert_eq!(result1.1, PurityMode::Override);
    }

    #[test]
    fn test_compute_runtime() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "locked": {
                        "type": "github",
                        "narHash": "sha256-test"
                    }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(json.as_bytes()).unwrap();

        let project = Project::new("test-project");
        let options = CompilerOptions {
            purity_mode: PurityMode::Strict,
            flake_lock_path: Some(temp_file.path().to_path_buf()),
            ..Default::default()
        };

        let compiler = Compiler::with_options(project, options);
        let runtime = compiler
            .compute_runtime(
                "nix-x86_64-linux",
                "github:NixOS/nixpkgs",
                "devShells.x86_64-linux.default",
                "x86_64-linux",
            )
            .unwrap();

        assert_eq!(runtime.id, "nix-x86_64-linux");
        assert_eq!(runtime.flake, "github:NixOS/nixpkgs");
        assert!(runtime.digest.starts_with("sha256:"));
        assert_eq!(runtime.purity, PurityMode::Strict);
    }
}
