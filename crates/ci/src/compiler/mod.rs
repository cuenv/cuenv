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
    ArtifactDownload, BuildStage, CachePolicy, IntermediateRepresentation, IrValidator,
    ManualTriggerConfig, OutputDeclaration, OutputType, PurityMode, Runtime, SecretConfig,
    StageTask, Task as IrTask, TriggerCondition, WorkflowDispatchInputDef,
};
use crate::stages;
use cuenv_core::ci::{
    BuildStage as CueBuildStage, CI, Contributor, CueStageTask, ManualTrigger, Pipeline,
    PipelineTask, SecretRef, SetupStep, StageContributor,
};
use cuenv_core::manifest::Project;
use cuenv_core::tasks::{Task, TaskDefinition, TaskGroup};
use digest::DigestBuilder;
use std::collections::{BTreeMap, HashMap, HashSet};
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

/// Factory function type for creating stage contributors.
///
/// Used to defer contributor creation until compilation time,
/// allowing `CompilerOptions` to remain `Clone`.
pub type ContributorFactory = fn() -> Vec<Box<dyn crate::StageContributor>>;

/// Compiler configuration options
#[derive(Clone, Default)]
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

    /// Factory function for creating stage contributors.
    ///
    /// If None, uses `stages::default_contributors()`.
    /// Set this to include provider-specific contributors (e.g., Cachix for GitHub).
    pub contributor_factory: Option<ContributorFactory>,

    /// Enable CI mode for orchestrator artifact handling.
    ///
    /// When true:
    /// - Task outputs use `OutputType::Orchestrator` for cross-job artifact sharing
    /// - Task input references (`inputs: [{task: "..."}]`) are converted to `artifact_downloads`
    pub ci_mode: bool,

    /// Module root (repo root / cue.mod location)
    ///
    /// Used for constructing trigger paths relative to the repository root.
    pub module_root: Option<PathBuf>,

    /// Project path relative to module root
    ///
    /// Used as a fallback for trigger paths when tasks have no explicit inputs.
    /// For example, if a project is at `projects/rawkode.academy/api`, this would
    /// be `"projects/rawkode.academy/api"`.
    pub project_path: Option<String>,
}

impl std::fmt::Debug for CompilerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompilerOptions")
            .field("purity_mode", &self.purity_mode)
            .field("validate_inputs", &self.validate_inputs)
            .field("default_cache_policy", &self.default_cache_policy)
            .field("flake_lock_path", &self.flake_lock_path)
            .field("project_root", &self.project_root)
            .field("input_overrides", &self.input_overrides)
            .field("pipeline", &self.pipeline)
            .field(
                "contributor_factory",
                &self.contributor_factory.map(|_| "Some(<fn>)"),
            )
            .field("ci_mode", &self.ci_mode)
            .field("module_root", &self.module_root)
            .field("project_path", &self.project_path)
            .finish()
    }
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

        // Set up trigger conditions from CI configuration using the pipeline from options
        if let Some(ref pipeline) = self.options.pipeline
            && let Some(ci_config) = &self.project.ci
        {
            ir.pipeline.trigger = Some(self.build_trigger_condition(pipeline, ci_config));
        }

        // Compile tasks
        self.compile_tasks(&self.project.tasks, &mut ir)?;

        // Fix artifact download paths to use actual upstream task outputs
        // (artifact_downloads are initially created with paths derived from task names,
        // but should use the actual output paths from the upstream tasks)
        Self::fix_artifact_download_paths(&mut ir);

        // Apply stage contributors with fixed-point iteration
        // Contributors self-detect their requirements and report modifications.
        // Loop continues until no contributor reports changes (stable state).
        let contributors = self
            .options
            .contributor_factory
            .map_or_else(stages::default_contributors, |factory| factory());
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
            ir.stages.sort_by_dependencies();
            if !any_modified {
                break;
            }
        }

        // Apply CUE-defined setup steps and legacy contributors
        self.apply_cue_setup_steps(&mut ir);

        // Apply CUE-defined stage contributors (v1.4+)
        self.apply_cue_stage_contributors(&mut ir);

        // Re-sort by dependencies after adding CUE setup steps
        ir.stages.sort_by_dependencies();

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
    fn build_trigger_condition(&self, pipeline: &Pipeline, _ci_config: &CI) -> TriggerCondition {
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
                inputs: BTreeMap::new(),
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

        // Note: paths_ignore is platform-specific (e.g., GitHub's paths-ignore).
        // It should be populated by the platform emitter, not the abstract compiler.
        let paths_ignore = Vec::new();

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
        let mut task_inputs = HashSet::new();

        // Collect inputs from all pipeline tasks (including transitive deps)
        // These paths are relative to the project directory
        for task in &pipeline.tasks {
            self.collect_task_inputs(task.task_name(), &mut task_inputs);
        }

        let mut paths = HashSet::new();

        // Prefix task inputs with project_path to make them relative to module root
        if let Some(project_path) = &self.options.project_path {
            for input in &task_inputs {
                paths.insert(format!("{project_path}/{input}"));
            }
        } else {
            paths.extend(task_inputs.clone());
        }

        // If no task inputs were collected, use the project directory as fallback
        // This ensures workflows trigger on any file change within the project
        if task_inputs.is_empty() {
            if let Some(project_path) = &self.options.project_path {
                paths.insert(format!("{project_path}/**"));
            } else {
                // Root project with no inputs - use wildcard for all files
                paths.insert("**".to_string());
            }
        }

        // Add implicit CUE inputs (changes here should always trigger)
        // Prefix with project_path if set
        if let Some(project_path) = &self.options.project_path {
            paths.insert(format!("{project_path}/env.cue"));
            paths.insert(format!("{project_path}/schema/**"));
        } else {
            paths.insert("env.cue".to_string());
            paths.insert("schema/**".to_string());
        }
        // cue.mod is always at module root, not prefixed with project_path
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
        let env: BTreeMap<String, String> = task
            .env
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect();

        // Extract secrets (simplified - would integrate with secret resolver)
        let secrets: BTreeMap<String, SecretConfig> = BTreeMap::new();

        // Convert inputs (path globs only for now)
        let inputs: Vec<String> = task.iter_path_inputs().cloned().collect();

        // Convert outputs - use Orchestrator in CI mode for cross-job artifact sharing
        let output_type = if self.options.ci_mode {
            OutputType::Orchestrator
        } else {
            OutputType::Cas
        };
        let outputs: Vec<OutputDeclaration> = task
            .outputs
            .iter()
            .map(|path| OutputDeclaration {
                path: path.clone(),
                output_type,
            })
            .collect();

        // Convert task output references to artifact downloads (CI mode only)
        let artifact_downloads: Vec<ArtifactDownload> = if self.options.ci_mode {
            task.iter_task_outputs()
                .map(|task_ref| {
                    // Use the task name to construct artifact name
                    // The path should match where the artifact was uploaded from
                    ArtifactDownload {
                        name: format!("{}-artifacts", task_ref.task.replace('.', "-")),
                        path: task_ref.task.replace('.', "/"),
                        filter: String::new(),
                    }
                })
                .collect()
        } else {
            vec![]
        };

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
            artifact_downloads,
            params: BTreeMap::new(),
        })
    }

    /// Fix artifact download paths to use actual upstream task output paths.
    ///
    /// During initial compilation, artifact_downloads are created with paths derived
    /// from task names (e.g., "docs.build" → "docs/build"). This post-processing step
    /// updates those paths to use the actual output paths from the upstream tasks
    /// (e.g., "docs/dist" if that's what docs.build outputs).
    fn fix_artifact_download_paths(ir: &mut IntermediateRepresentation) {
        // Build a lookup map: task_id → first output path
        // We use the first output path as the download destination
        let task_outputs: HashMap<String, String> = ir
            .tasks
            .iter()
            .filter_map(|task| {
                task.outputs
                    .first()
                    .map(|output| (task.id.clone(), output.path.clone()))
            })
            .collect();

        // Update artifact downloads to use actual upstream output paths
        for task in &mut ir.tasks {
            for download in &mut task.artifact_downloads {
                // Extract task ID from artifact name (e.g., "docs-build-artifacts" → "docs.build")
                let upstream_task_id = download
                    .name
                    .strip_suffix("-artifacts")
                    .map(|s| s.replace('-', "."))
                    .unwrap_or_default();

                // If we have the upstream task's output path, use it
                if let Some(output_path) = task_outputs.get(&upstream_task_id) {
                    download.path.clone_from(output_path);
                }
            }
        }
    }

    /// Apply CUE-defined setup steps and contributors to the IR
    ///
    /// This processes:
    /// 1. Inline setup steps defined on the pipeline
    /// 2. CUE contributors whose `when` condition matches pipeline tasks
    fn apply_cue_setup_steps(&self, ir: &mut IntermediateRepresentation) {
        // 1. Add pipeline-level setup steps
        if let Some(ref pipeline) = self.options.pipeline {
            for step in &pipeline.setup {
                let stage_task = Self::setup_step_to_stage_task(step, "pipeline");
                ir.stages.add(BuildStage::Setup, stage_task);
            }
        }

        // 2. Apply CUE contributors (sorted by name for deterministic order)
        if let Some(ref ci_config) = self.project.ci {
            // Sort contributor names for deterministic iteration order
            let mut contributor_names: Vec<_> = ci_config.contributors.keys().collect();
            contributor_names.sort();

            for name in contributor_names {
                let contributor = &ci_config.contributors[name];
                if self.contributor_matches(contributor) {
                    for step in &contributor.setup {
                        let stage_task = Self::setup_step_to_stage_task(step, name);
                        ir.stages.add(BuildStage::Setup, stage_task);
                    }
                }
            }
        }
    }

    /// Convert a CUE SetupStep to an IR StageTask
    fn setup_step_to_stage_task(step: &SetupStep, provider: &str) -> StageTask {
        // Build command array from command+args or script
        let (command, shell) = if let Some(ref cmd) = step.command {
            let mut cmd_vec = vec![cmd.clone()];
            cmd_vec.extend(step.args.clone());
            (cmd_vec, false)
        } else if let Some(ref script) = step.script {
            (
                vec!["/bin/sh".to_string(), "-c".to_string(), script.clone()],
                true,
            )
        } else {
            // Empty command - shouldn't happen with valid CUE
            (vec![], false)
        };

        // Convert env values (filter to strings only)
        let env: BTreeMap<String, String> = step
            .env
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect();

        // Convert provider-specific overrides to provider_hints
        let provider_hints = step.provider.as_ref().and_then(|p| {
            p.github.as_ref().map(|gh| {
                let mut github_action = serde_json::Map::new();
                github_action.insert(
                    "uses".to_string(),
                    serde_json::Value::String(gh.uses.clone()),
                );
                if !gh.inputs.is_empty() {
                    github_action.insert(
                        "inputs".to_string(),
                        serde_json::Value::Object(
                            gh.inputs
                                .iter()
                                .map(|(k, v)| (k.clone(), v.clone()))
                                .collect(),
                        ),
                    );
                }

                let mut hints = serde_json::Map::new();
                hints.insert(
                    "github_action".to_string(),
                    serde_json::Value::Object(github_action),
                );
                serde_json::Value::Object(hints)
            })
        });

        StageTask {
            id: format!("cue-setup-{}", step.name.to_lowercase().replace(' ', "-")),
            provider: format!("cue:{provider}"),
            label: Some(step.name.clone()),
            command,
            shell,
            env,
            secrets: BTreeMap::new(),
            depends_on: vec![],
            priority: 50, // CUE setup steps run after Rust contributors (which use 10-30)
            provider_hints,
        }
    }

    /// Check if a CUE contributor's `when` condition matches any pipeline task
    fn contributor_matches(&self, contributor: &Contributor) -> bool {
        let Some(ref when) = contributor.when else {
            // No `when` condition = always active
            return true;
        };

        // Get the pipeline tasks we're checking against
        let Some(ref pipeline) = self.options.pipeline else {
            return false;
        };

        // Check if any pipeline task matches the contributor's conditions
        for pipeline_task in &pipeline.tasks {
            let task_name = pipeline_task.task_name();
            if let Some(task) = self.find_task(task_name)
                && Self::task_matches_condition(task, when)
            {
                return true;
            }
        }

        false
    }

    /// Check if a task matches a TaskMatcher condition
    fn task_matches_condition(task: &Task, condition: &cuenv_core::manifest::TaskMatcher) -> bool {
        // Check labels (all must match)
        if let Some(ref required_labels) = condition.labels {
            let has_all_labels = required_labels
                .iter()
                .all(|label| task.labels.contains(label));
            if !has_all_labels {
                return false;
            }
        }

        // Check command
        if let Some(ref required_cmd) = condition.command
            && &task.command != required_cmd
        {
            return false;
        }

        // Check args patterns
        if let Some(ref arg_matchers) = condition.args {
            for arg_cond in arg_matchers {
                let is_match = if let Some(ref contains) = arg_cond.contains {
                    task.args.iter().any(|arg| arg.contains(contains))
                } else if let Some(ref pattern) = arg_cond.matches {
                    // Regex pattern matching
                    if let Ok(re) = regex::Regex::new(pattern) {
                        task.args.iter().any(|arg| re.is_match(arg))
                    } else {
                        false
                    }
                } else {
                    true // No conditions = matches
                };

                if !is_match {
                    return false;
                }
            }
        }

        // Note: workspaces matching would require additional context
        // For now, we only match on labels, command, and args

        true
    }

    // =========================================================================
    // CUE Stage Contributors (v1.4)
    // =========================================================================

    /// Apply CUE-defined stage contributors to the IR
    ///
    /// This evaluates the `ci.stageContributors` array from CUE and converts
    /// active contributors' tasks to IR stage tasks.
    fn apply_cue_stage_contributors(&self, ir: &mut IntermediateRepresentation) {
        let Some(ref ci_config) = self.project.ci else {
            return;
        };

        for contributor in &ci_config.stage_contributors {
            // Check if this contributor is active
            if !self.cue_stage_contributor_is_active(contributor, ir) {
                continue;
            }

            // Already contributed check (idempotency)
            let contributed_ids: HashSet<String> = ir
                .stages
                .bootstrap
                .iter()
                .chain(ir.stages.setup.iter())
                .chain(ir.stages.success.iter())
                .chain(ir.stages.failure.iter())
                .map(|t| t.id.clone())
                .collect();

            // Add contributor's tasks to appropriate stages
            for cue_task in &contributor.tasks {
                // Skip if already contributed
                if contributed_ids.contains(&cue_task.id) {
                    continue;
                }

                let stage_task = Self::cue_stage_task_to_ir(cue_task, &contributor.id);
                let stage = Self::cue_build_stage_to_ir(cue_task.stage);
                ir.stages.add(stage, stage_task);
            }
        }
    }

    /// Check if a CUE stage contributor is active
    fn cue_stage_contributor_is_active(
        &self,
        contributor: &StageContributor,
        ir: &IntermediateRepresentation,
    ) -> bool {
        let Some(ref condition) = contributor.when else {
            // No condition = always active
            return true;
        };

        // Check always condition
        if condition.always == Some(true) {
            return true;
        }

        // Check runtime type
        if !condition.runtime_type.is_empty() {
            if let Some(ref runtime) = self.project.runtime {
                let runtime_type = Self::get_runtime_type(runtime);
                if !condition.runtime_type.iter().any(|t| t == runtime_type) {
                    return false;
                }
            } else {
                // No runtime set but runtime type condition exists
                return false;
            }
        }

        // Check cuenv source mode
        if !condition.cuenv_source.is_empty() {
            let source = self
                .project
                .config
                .as_ref()
                .and_then(|c| c.ci.as_ref())
                .and_then(|ci| ci.cuenv.as_ref())
                .map_or("release", |c| c.source.as_str());
            if !condition.cuenv_source.iter().any(|s| s == source) {
                return false;
            }
        }

        // Check secrets provider
        if !condition.secrets_provider.is_empty()
            && !self.has_secrets_provider(&condition.secrets_provider, ir)
        {
            return false;
        }

        // Check provider config
        if !condition.provider_config.is_empty()
            && !self.has_provider_config(&condition.provider_config)
        {
            return false;
        }

        // Check task command
        if !condition.task_command.is_empty()
            && !Self::has_task_command(&condition.task_command, ir)
        {
            return false;
        }

        // Check task labels
        if !condition.task_labels.is_empty() && !self.has_task_labels(&condition.task_labels) {
            return false;
        }

        // Check environment
        if !condition.environment.is_empty() {
            let Some(ref pipeline) = self.options.pipeline else {
                return false;
            };
            let Some(ref env_name) = pipeline.environment else {
                return false;
            };
            if !condition.environment.iter().any(|e| e == env_name) {
                return false;
            }
        }

        true
    }

    /// Get the runtime type string for condition matching
    fn get_runtime_type(runtime: &cuenv_core::manifest::Runtime) -> &'static str {
        match runtime {
            cuenv_core::manifest::Runtime::Nix(_) => "nix",
            cuenv_core::manifest::Runtime::Devenv(_) => "devenv",
            cuenv_core::manifest::Runtime::Container(_) => "container",
            cuenv_core::manifest::Runtime::Dagger(_) => "dagger",
            cuenv_core::manifest::Runtime::Oci(_) => "oci",
            cuenv_core::manifest::Runtime::Tools(_) => "tools",
        }
    }

    /// Check if the pipeline environment uses any of the specified secrets providers
    fn has_secrets_provider(
        &self,
        providers: &[String],
        ir: &IntermediateRepresentation,
    ) -> bool {
        let Some(ref env_name) = ir.pipeline.environment else {
            return false;
        };
        let Some(ref env) = self.project.env else {
            return false;
        };

        // Check for provider references in the environment
        let env_vars = env.for_environment(env_name);
        for value in env_vars.values() {
            // Check for 1Password references
            if providers.iter().any(|p| p == "onepassword") {
                match value {
                    cuenv_core::environment::EnvValue::String(s) if s.starts_with("op://") => {
                        return true;
                    }
                    cuenv_core::environment::EnvValue::Secret(secret)
                        if secret.resolver == "onepassword" =>
                    {
                        return true;
                    }
                    cuenv_core::environment::EnvValue::WithPolicies(wp) => {
                        match &wp.value {
                            cuenv_core::environment::EnvValueSimple::Secret(secret)
                                if secret.resolver == "onepassword" =>
                            {
                                return true;
                            }
                            cuenv_core::environment::EnvValueSimple::String(s)
                                if s.starts_with("op://") =>
                            {
                                return true;
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            // Add other provider checks as needed (aws, vault, etc.)
        }
        false
    }

    /// Check if any of the specified provider config paths are set
    fn has_provider_config(&self, paths: &[String]) -> bool {
        let Some(ref ci) = self.project.ci else {
            return false;
        };
        let Some(ref provider) = ci.provider else {
            return false;
        };

        for path in paths {
            let parts: Vec<&str> = path.split('.').collect();
            if parts.is_empty() {
                continue;
            }

            // Get the top-level provider config (e.g., "github")
            let Some(config) = provider.get(parts[0]) else {
                continue;
            };

            // Navigate the path
            let mut current = config;
            let mut found = true;
            for part in &parts[1..] {
                match current.get(*part) {
                    Some(value) if !value.is_null() => {
                        current = value;
                    }
                    _ => {
                        found = false;
                        break;
                    }
                }
            }

            if found {
                return true;
            }
        }

        false
    }

    /// Check if any pipeline task uses the specified command
    fn has_task_command(commands: &[String], ir: &IntermediateRepresentation) -> bool {
        // Check IR tasks for command matches
        for task in &ir.tasks {
            // Only check tasks in the pipeline
            if !ir.pipeline.pipeline_tasks.is_empty()
                && !ir.pipeline.pipeline_tasks.contains(&task.id)
            {
                continue;
            }

            // Check if command starts with the specified commands
            if task.command.len() >= commands.len() {
                let matches = commands
                    .iter()
                    .zip(task.command.iter())
                    .all(|(a, b)| a == b);
                if matches {
                    return true;
                }
            }

            // Also check shell commands for the pattern
            if task.shell && task.command.len() == 1 {
                let cmd_str = commands.join(" ");
                if task.command[0].contains(&cmd_str) {
                    return true;
                }
            }
        }

        false
    }

    /// Check if any pipeline task has the specified labels
    fn has_task_labels(&self, labels: &[String]) -> bool {
        let Some(ref pipeline) = self.options.pipeline else {
            return false;
        };

        for pipeline_task in &pipeline.tasks {
            let task_name = pipeline_task.task_name();
            if let Some(task) = self.find_task(task_name) {
                let has_all = labels.iter().all(|l| task.labels.contains(l));
                if has_all {
                    return true;
                }
            }
        }

        false
    }

    /// Convert a CUE BuildStage to IR BuildStage
    fn cue_build_stage_to_ir(stage: CueBuildStage) -> BuildStage {
        match stage {
            CueBuildStage::Bootstrap => BuildStage::Bootstrap,
            CueBuildStage::Setup => BuildStage::Setup,
            CueBuildStage::Success => BuildStage::Success,
            CueBuildStage::Failure => BuildStage::Failure,
        }
    }

    /// Convert a CUE stage task to an IR StageTask
    fn cue_stage_task_to_ir(cue_task: &CueStageTask, provider: &str) -> StageTask {
        // Build command array
        let (command, shell) = if let Some(ref cmd) = cue_task.command {
            (vec![cmd.clone()], cue_task.shell)
        } else if let Some(ref script) = cue_task.script {
            (vec![script.clone()], true)
        } else {
            (vec![], false)
        };

        // Convert secrets
        let secrets: BTreeMap<String, SecretConfig> = cue_task
            .secrets
            .iter()
            .map(|(k, v)| {
                let config = match v {
                    SecretRef::Simple(s) => SecretConfig {
                        source: s.clone(),
                        cache_key: false,
                    },
                    SecretRef::Detailed(d) => SecretConfig {
                        source: d.source.clone(),
                        cache_key: d.cache_key,
                    },
                };
                (k.clone(), config)
            })
            .collect();

        // Convert provider hints
        let provider_hints = cue_task.provider.as_ref().and_then(|p| {
            p.github.as_ref().map(|gh| {
                let mut github_action = serde_json::Map::new();
                github_action.insert(
                    "uses".to_string(),
                    serde_json::Value::String(gh.uses.clone()),
                );
                if !gh.inputs.is_empty() {
                    github_action.insert(
                        "inputs".to_string(),
                        serde_json::Value::Object(
                            gh.inputs
                                .iter()
                                .map(|(k, v)| (k.clone(), v.clone()))
                                .collect(),
                        ),
                    );
                }

                let mut hints = serde_json::Map::new();
                hints.insert(
                    "github_action".to_string(),
                    serde_json::Value::Object(github_action),
                );
                serde_json::Value::Object(hints)
            })
        });

        StageTask {
            id: cue_task.id.clone(),
            provider: provider.to_string(),
            label: cue_task.label.clone(),
            command,
            shell,
            env: cue_task
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            secrets,
            depends_on: cue_task.depends_on.clone(),
            priority: cue_task.priority,
            provider_hints,
        }
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

    #[test]
    fn test_derive_trigger_paths_with_project_path() {
        use cuenv_core::ci::{CI, Pipeline, PipelineCondition, PipelineTask, StringOrVec};

        let mut project = Project::new("test-project");
        project.tasks.insert(
            "build".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo".to_string(),
                args: vec!["build".to_string()],
                inputs: vec![
                    cuenv_core::tasks::Input::Path("src/**/*.rs".to_string()),
                    cuenv_core::tasks::Input::Path("Cargo.toml".to_string()),
                ],
                ..Default::default()
            })),
        );

        let pipeline = Pipeline {
            name: "default".to_string(),
            environment: None,
            setup: vec![],
            tasks: vec![PipelineTask::Simple("build".to_string())],
            when: Some(PipelineCondition {
                branch: Some(StringOrVec::String("main".to_string())),
                pull_request: None,
                tag: None,
                default_branch: None,
                scheduled: None,
                manual: None,
                release: None,
            }),
            derive_paths: None,
            provider: None,
        };

        // Add CI config with a pipeline
        project.ci = Some(CI {
            pipelines: vec![pipeline.clone()],
            ..Default::default()
        });

        let options = CompilerOptions {
            pipeline: Some(pipeline),
            project_path: Some("projects/api".to_string()),
            ..Default::default()
        };

        let compiler = Compiler::with_options(project, options);
        let ir = compiler.compile().unwrap();

        let trigger = ir.pipeline.trigger.expect("should have trigger");

        // Task inputs should be prefixed with project_path
        assert!(
            trigger
                .paths
                .contains(&"projects/api/src/**/*.rs".to_string())
        );
        assert!(
            trigger
                .paths
                .contains(&"projects/api/Cargo.toml".to_string())
        );

        // CUE implicit paths should also be prefixed
        assert!(trigger.paths.contains(&"projects/api/env.cue".to_string()));
        assert!(
            trigger
                .paths
                .contains(&"projects/api/schema/**".to_string())
        );

        // cue.mod should NOT be prefixed (it's at module root)
        assert!(trigger.paths.contains(&"cue.mod/**".to_string()));
    }

    #[test]
    fn test_derive_trigger_paths_fallback_to_project_dir() {
        use cuenv_core::ci::{CI, Pipeline, PipelineCondition, PipelineTask, StringOrVec};

        let mut project = Project::new("test-project");
        // Task with NO inputs
        project.tasks.insert(
            "deploy".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "kubectl".to_string(),
                args: vec!["apply".to_string()],
                ..Default::default()
            })),
        );

        let pipeline = Pipeline {
            name: "default".to_string(),
            environment: None,
            setup: vec![],
            tasks: vec![PipelineTask::Simple("deploy".to_string())],
            when: Some(PipelineCondition {
                branch: Some(StringOrVec::String("main".to_string())),
                pull_request: None,
                tag: None,
                default_branch: None,
                scheduled: None,
                manual: None,
                release: None,
            }),
            derive_paths: None,
            provider: None,
        };

        project.ci = Some(CI {
            pipelines: vec![pipeline.clone()],
            ..Default::default()
        });

        let options = CompilerOptions {
            pipeline: Some(pipeline),
            project_path: Some("projects/rawkode.academy/api".to_string()),
            ..Default::default()
        };

        let compiler = Compiler::with_options(project, options);
        let ir = compiler.compile().unwrap();

        let trigger = ir.pipeline.trigger.expect("should have trigger");

        // When no task inputs, should fallback to project directory
        assert!(
            trigger
                .paths
                .contains(&"projects/rawkode.academy/api/**".to_string()),
            "Should contain fallback path. Paths: {:?}",
            trigger.paths
        );
    }

    #[test]
    fn test_derive_trigger_paths_root_project() {
        use cuenv_core::ci::{CI, Pipeline, PipelineCondition, PipelineTask, StringOrVec};

        let mut project = Project::new("test-project");
        project.tasks.insert(
            "build".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo".to_string(),
                args: vec!["build".to_string()],
                inputs: vec![cuenv_core::tasks::Input::Path("src/**".to_string())],
                ..Default::default()
            })),
        );

        let pipeline = Pipeline {
            name: "default".to_string(),
            environment: None,
            setup: vec![],
            tasks: vec![PipelineTask::Simple("build".to_string())],
            when: Some(PipelineCondition {
                branch: Some(StringOrVec::String("main".to_string())),
                pull_request: None,
                tag: None,
                default_branch: None,
                scheduled: None,
                manual: None,
                release: None,
            }),
            derive_paths: None,
            provider: None,
        };

        project.ci = Some(CI {
            pipelines: vec![pipeline.clone()],
            ..Default::default()
        });

        // No project_path = root project
        let options = CompilerOptions {
            pipeline: Some(pipeline),
            project_path: None,
            ..Default::default()
        };

        let compiler = Compiler::with_options(project, options);
        let ir = compiler.compile().unwrap();

        let trigger = ir.pipeline.trigger.expect("should have trigger");

        // Paths should NOT be prefixed for root projects
        assert!(trigger.paths.contains(&"src/**".to_string()));
        assert!(trigger.paths.contains(&"env.cue".to_string()));
        assert!(trigger.paths.contains(&"schema/**".to_string()));
    }

    #[test]
    fn test_derive_trigger_paths_root_project_no_inputs_fallback() {
        use cuenv_core::ci::{CI, Pipeline, PipelineCondition, PipelineTask, StringOrVec};

        let mut project = Project::new("test-project");
        // Task with NO inputs
        project.tasks.insert(
            "deploy".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "kubectl".to_string(),
                args: vec!["apply".to_string()],
                ..Default::default()
            })),
        );

        let pipeline = Pipeline {
            name: "default".to_string(),
            environment: None,
            setup: vec![],
            tasks: vec![PipelineTask::Simple("deploy".to_string())],
            when: Some(PipelineCondition {
                branch: Some(StringOrVec::String("main".to_string())),
                pull_request: None,
                tag: None,
                default_branch: None,
                scheduled: None,
                manual: None,
                release: None,
            }),
            derive_paths: None,
            provider: None,
        };

        project.ci = Some(CI {
            pipelines: vec![pipeline.clone()],
            ..Default::default()
        });

        // No project_path = root project
        let options = CompilerOptions {
            pipeline: Some(pipeline),
            project_path: None,
            ..Default::default()
        };

        let compiler = Compiler::with_options(project, options);
        let ir = compiler.compile().unwrap();

        let trigger = ir.pipeline.trigger.expect("should have trigger");

        // Root project with no inputs should fallback to **
        assert!(
            trigger.paths.contains(&"**".to_string()),
            "Root project with no inputs should fallback to **. Paths: {:?}",
            trigger.paths
        );
    }
}
