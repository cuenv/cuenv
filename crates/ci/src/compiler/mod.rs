//! Compiler from cuenv task definitions to IR v1.5
//!
//! Transforms a cuenv `Project` with tasks into an intermediate representation
//! suitable for emitting orchestrator-native CI configurations.
//!
//! ## Contributors
//!
//! Contributors are defined in CUE (see `contrib/contributors/`). The compiler
//! evaluates the `ci.contributors` array and injects active contributors'
//! tasks into the appropriate build phases.

// IR compilation involves complex transformations with many fields
#![allow(clippy::too_many_lines)]

pub mod digest;

use crate::flake::{FlakeLockAnalyzer, FlakeLockError, PurityAnalysis};
use crate::ir::{
    ArtifactDownload, BuildStage, CachePolicy, IntermediateRepresentation, IrValidator,
    ManualTriggerConfig, OutputDeclaration, OutputType, PurityMode, Runtime, SecretConfig,
    Task as IrTask, TaskCondition, TriggerCondition, WorkflowDispatchInputDef,
};
use cuenv_core::ci::{
    CI, Contributor, ContributorTask, ManualTrigger, Pipeline, PipelineTask, SecretRef,
    TaskCondition as CueTaskCondition,
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

    #[error("Task '{0}' not found")]
    TaskNotFound(String),

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

    /// Pipeline name (for environment-aware compilation)
    ///
    /// When set, this is used for workflow naming and identification.
    pub pipeline_name: Option<String>,

    /// Pipeline being compiled (for environment-aware compilation)
    ///
    /// When set, the compiler will set `ir.pipeline.environment` from
    /// the pipeline's environment, enabling CUE stage contributors to
    /// evaluate their activation conditions.
    pub pipeline: Option<Pipeline>,

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
            .field("pipeline_name", &self.pipeline_name)
            .field("pipeline", &self.pipeline)
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
            // Preserve full pipeline task definitions (including matrix configs)
            ir.pipeline.pipeline_task_defs.clone_from(&pipeline.tasks);
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

        // Apply CUE-defined contributors
        self.apply_cue_contributors(&mut ir);

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

    /// Compile a single task by name to IR
    ///
    /// This method handles both single tasks and task groups, compiling them
    /// into an IR representation that can be executed.
    ///
    /// # Errors
    ///
    /// Returns `CompilerError` if the task is not found or compilation fails.
    pub fn compile_task(
        &self,
        task_name: &str,
    ) -> Result<IntermediateRepresentation, CompilerError> {
        let mut ir = IntermediateRepresentation::new(&self.project.name);

        // Find the task definition
        let Some(task_def) = self.find_task_definition(task_name) else {
            return Err(CompilerError::TaskNotFound(task_name.to_string()));
        };

        // Compile just this task definition
        self.compile_task_definition(task_name, task_def, &mut ir)?;

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

        // Helper to prefix a path with project_path, handling "." (root) specially
        // GitHub Actions doesn't handle "./" prefix correctly
        let prefix_path = |path: &str| -> String {
            match &self.options.project_path {
                Some(pp) if pp == "." => path.to_string(),
                Some(pp) => format!("{pp}/{path}"),
                None => path.to_string(),
            }
        };

        // Add task inputs with appropriate prefix
        for input in &task_inputs {
            paths.insert(prefix_path(input));
        }

        // If no task inputs were collected, use the project directory as fallback
        // This ensures workflows trigger on any file change within the project
        if task_inputs.is_empty() {
            match &self.options.project_path {
                Some(pp) if pp == "." => paths.insert("**".to_string()),
                Some(pp) => paths.insert(format!("{pp}/**")),
                None => paths.insert("**".to_string()),
            };
        }

        // Add implicit CUE inputs (changes here should always trigger)
        paths.insert(prefix_path("env.cue"));
        paths.insert(prefix_path("schema/**"));
        // cue.mod is always at module root, not prefixed with project_path
        paths.insert("cue.mod/**".to_string());

        // Sort for deterministic output
        let mut result: Vec<_> = paths.into_iter().collect();
        result.sort();
        result
    }

    /// Recursively collect task inputs including dependencies
    fn collect_task_inputs(&self, task_name: &str, paths: &mut HashSet<String>) {
        if let Some(def) = self.find_task_definition(task_name) {
            self.collect_inputs_from_definition(def, paths);
        }
    }

    /// Collect inputs from a task definition (handles both single tasks and groups)
    fn collect_inputs_from_definition(&self, def: &TaskDefinition, paths: &mut HashSet<String>) {
        match def {
            TaskDefinition::Single(task) => {
                // Add direct inputs
                for input in task.iter_path_inputs() {
                    paths.insert(input.clone());
                }
                // Recurse into dependencies
                for dep in &task.depends_on {
                    self.collect_task_inputs(dep, paths);
                }
            }
            TaskDefinition::Group(group) => {
                self.collect_inputs_from_group(group, paths);
            }
        }
    }

    /// Collect inputs from all tasks in a group
    fn collect_inputs_from_group(&self, group: &TaskGroup, paths: &mut HashSet<String>) {
        match group {
            TaskGroup::Parallel(parallel) => {
                for def in parallel.tasks.values() {
                    self.collect_inputs_from_definition(def, paths);
                }
            }
            TaskGroup::Sequential(seq) => {
                for def in seq {
                    self.collect_inputs_from_definition(def, paths);
                }
            }
        }
    }

    /// Find a task definition by name (handles dotted paths for nested tasks)
    /// Returns the TaskDefinition which can be either a single task or a group
    fn find_task_definition(&self, name: &str) -> Option<&TaskDefinition> {
        let parts: Vec<&str> = name.split('.').collect();
        let mut current_tasks = &self.project.tasks;

        for (i, part) in parts.iter().enumerate() {
            match current_tasks.get(*part) {
                Some(def) if i == parts.len() - 1 => {
                    return Some(def);
                }
                Some(TaskDefinition::Group(TaskGroup::Parallel(parallel))) => {
                    current_tasks = &parallel.tasks;
                }
                _ => return None,
            }
        }
        None
    }

    /// Find a leaf task by name (handles dotted paths for nested tasks)
    fn find_task(&self, name: &str) -> Option<&Task> {
        match self.find_task_definition(name) {
            Some(TaskDefinition::Single(task)) => Some(task),
            _ => None,
        }
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
            // Phase task fields (not applicable for regular tasks)
            phase: None,
            label: None,
            priority: None,
            contributor: None,
            condition: None,
            provider_hints: None,
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

    /// Apply CUE-defined contributors to the IR
    ///
    /// This evaluates the `ci.contributors` array from CUE and converts
    /// active contributors' tasks to IR contributor tasks.
    fn apply_cue_contributors(&self, ir: &mut IntermediateRepresentation) {
        let Some(ref ci_config) = self.project.ci else {
            return;
        };

        for contributor in &ci_config.contributors {
            // Check if this contributor is active
            if !self.cue_contributor_is_active(contributor, ir) {
                continue;
            }

            // Already contributed check (idempotency) - check all contributor tasks
            let contributed_ids: HashSet<String> = ir
                .tasks
                .iter()
                .filter(|t| t.phase.is_some())
                .map(|t| t.id.clone())
                .collect();

            // Add contributor's tasks
            for contributor_task in &contributor.tasks {
                // Prefix the task ID with contributor namespace
                let full_task_id = format!("cuenv:contributor:{}", contributor_task.id);

                // Skip if already contributed
                if contributed_ids.contains(&full_task_id) {
                    continue;
                }

                let task = Self::contributor_task_to_ir(contributor_task, &contributor.id);
                ir.tasks.push(task);
            }
        }
    }

    /// Check if a CUE contributor is active
    fn cue_contributor_is_active(
        &self,
        contributor: &Contributor,
        ir: &IntermediateRepresentation,
    ) -> bool {
        let Some(ref condition) = contributor.when else {
            // No condition = always active
            return true;
        };

        // Check always condition - if explicitly set, use its value directly
        if let Some(always_val) = condition.always {
            return always_val;
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

        // Check workspace membership (detect package managers using workspace discovery)
        if !condition.workspace_member.is_empty() {
            // Use module root for workspace detection (lockfiles are at repo root in monorepos)
            let module_root = self
                .options
                .module_root
                .clone()
                .or_else(|| self.options.project_root.clone())
                .unwrap_or_else(|| std::path::PathBuf::from("."));

            let detected = self.detect_workspace_managers(&module_root);

            if !condition
                .workspace_member
                .iter()
                .any(|t| detected.contains(&t.to_lowercase()))
            {
                return false;
            }
        }

        true
    }

    /// Detect package managers for the workspace, checking if the current project is a member.
    ///
    /// Uses `WorkspaceDiscovery` to properly discover workspace members rather than
    /// just checking for lockfiles in the current directory.
    fn detect_workspace_managers(&self, module_root: &std::path::Path) -> Vec<String> {
        use cuenv_workspaces::{PackageJsonDiscovery, WorkspaceDiscovery};

        // Try to discover workspace at module root
        if let Ok(workspace) = PackageJsonDiscovery.discover(module_root) {
            // Check if current project is a member of this workspace
            if let Some(ref project_path) = self.options.project_path {
                // Use contains_path() to check workspace membership
                let path = std::path::Path::new(project_path);
                if workspace.contains_path(path) || workspace.lockfile.is_some() {
                    return vec![workspace.manager.to_string().to_lowercase()];
                }
            } else {
                // No sub-project path means we're at root - use workspace manager directly
                return vec![workspace.manager.to_string().to_lowercase()];
            }
        }

        // Fallback to simple lockfile detection at module root
        cuenv_workspaces::detection::detect_package_managers(module_root)
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.to_string().to_lowercase())
            .collect()
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
    fn has_secrets_provider(&self, providers: &[String], ir: &IntermediateRepresentation) -> bool {
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
                    cuenv_core::environment::EnvValue::WithPolicies(wp) => match &wp.value {
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
                    },
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

    /// Derive the build stage from contributor task priority
    ///
    /// Priority ranges determine the stage:
    /// - 0-9: Bootstrap (environment setup like Nix)
    /// - 10-49: Setup (tool installation like cuenv, cachix)
    /// - 50+: Success phase (post-build tasks)
    ///
    /// Tasks with on_failure condition are placed in the Failure stage.
    fn derive_stage_from_priority(
        priority: i32,
        condition: Option<CueTaskCondition>,
    ) -> BuildStage {
        // on_failure condition always means Failure stage
        if matches!(condition, Some(CueTaskCondition::OnFailure)) {
            return BuildStage::Failure;
        }

        match priority {
            0..=9 => BuildStage::Bootstrap,
            10..=49 => BuildStage::Setup,
            _ => BuildStage::Success,
        }
    }

    /// Convert a CUE TaskCondition to an IR TaskCondition
    fn cue_task_condition_to_ir(condition: CueTaskCondition) -> TaskCondition {
        match condition {
            CueTaskCondition::OnSuccess => TaskCondition::OnSuccess,
            CueTaskCondition::OnFailure => TaskCondition::OnFailure,
            CueTaskCondition::Always => TaskCondition::Always,
        }
    }

    /// Convert a CUE ContributorTask to an IR Task
    ///
    /// Creates an IR Task with contributor metadata. Contributor tasks are stored
    /// alongside regular tasks in `ir.tasks` and distinguished by their `phase` field.
    /// The phase is derived from the task's priority.
    fn contributor_task_to_ir(contributor_task: &ContributorTask, contributor_id: &str) -> IrTask {
        // Build command array
        let (command, shell) = if let Some(ref cmd) = contributor_task.command {
            let mut cmd_vec = vec![cmd.clone()];
            cmd_vec.extend(contributor_task.args.clone());
            (cmd_vec, contributor_task.shell)
        } else if let Some(ref script) = contributor_task.script {
            (vec![script.clone()], true)
        } else {
            (vec![], false)
        };

        // Convert secrets
        let secrets: BTreeMap<String, SecretConfig> = contributor_task
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
        let provider_hints = contributor_task.provider.as_ref().and_then(|p| {
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

        // Convert condition
        let condition = contributor_task
            .condition
            .map(Self::cue_task_condition_to_ir);

        // Derive stage from priority
        let stage =
            Self::derive_stage_from_priority(contributor_task.priority, contributor_task.condition);

        // Prefix dependencies with contributor namespace
        let depends_on: Vec<String> = contributor_task
            .depends_on
            .iter()
            .map(|dep| {
                if dep.starts_with("cuenv:contributor:") {
                    dep.clone()
                } else {
                    format!("cuenv:contributor:{dep}")
                }
            })
            .collect();

        IrTask {
            id: format!("cuenv:contributor:{}", contributor_task.id),
            runtime: None,
            command,
            shell,
            env: contributor_task
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            secrets,
            resources: None,
            concurrency_group: None,
            inputs: contributor_task.inputs.clone(),
            outputs: vec![],
            depends_on,
            cache_policy: CachePolicy::Disabled, // Contributor tasks don't use caching
            deployment: false,
            manual_approval: false,
            matrix: None,
            artifact_downloads: vec![],
            params: BTreeMap::new(),
            // Contributor task specific fields
            phase: Some(stage),
            label: contributor_task.label.clone(),
            priority: Some(contributor_task.priority),
            contributor: Some(contributor_id.to_string()),
            condition,
            provider_hints,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::ci::PipelineMode;
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

        assert_eq!(ir.version, "1.5");
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
        use std::collections::BTreeMap;

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
            mode: PipelineMode::default(),
            environment: None,
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
            pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
            ..Default::default()
        });

        let options = CompilerOptions {
            pipeline_name: Some("default".to_string()),
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
        use std::collections::BTreeMap;

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
            mode: PipelineMode::default(),
            environment: None,
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
            pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
            ..Default::default()
        });

        let options = CompilerOptions {
            pipeline_name: Some("default".to_string()),
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
        use std::collections::BTreeMap;

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
            mode: PipelineMode::default(),
            environment: None,
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
            pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
            ..Default::default()
        });

        // No project_path = root project
        let options = CompilerOptions {
            pipeline_name: Some("default".to_string()),
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
        use std::collections::BTreeMap;

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
            mode: PipelineMode::default(),
            environment: None,
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
            pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
            ..Default::default()
        });

        // No project_path = root project
        let options = CompilerOptions {
            pipeline_name: Some("default".to_string()),
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

    // =========================================================================
    // Contributor Activation Tests
    // =========================================================================

    use cuenv_core::ci::ActivationCondition;
    use std::collections::HashMap;

    /// Helper to create a minimal Contributor for testing
    fn test_contributor(id: &str, when: Option<ActivationCondition>) -> Contributor {
        Contributor {
            id: id.to_string(),
            when,
            tasks: vec![],
            auto_associate: None,
        }
    }

    /// Helper to create a minimal IR for testing
    fn test_ir() -> IntermediateRepresentation {
        IntermediateRepresentation {
            version: "1.5".to_string(),
            pipeline: crate::ir::PipelineMetadata {
                name: "test".to_string(),
                mode: PipelineMode::default(),
                environment: None,
                requires_onepassword: false,
                project_name: None,
                trigger: None,
                pipeline_tasks: vec![],
                pipeline_task_defs: vec![],
            },
            runtimes: vec![],
            tasks: vec![],
        }
    }

    #[test]
    fn test_contributor_no_condition_always_active() {
        let project = Project::new("test");
        let compiler = Compiler::new(project);
        let ir = test_ir();

        // No `when` condition = always active
        let contributor = test_contributor("test", None);
        assert!(compiler.cue_contributor_is_active(&contributor, &ir));
    }

    #[test]
    fn test_contributor_always_true_active() {
        let project = Project::new("test");
        let compiler = Compiler::new(project);
        let ir = test_ir();

        let contributor = test_contributor(
            "test",
            Some(ActivationCondition {
                always: Some(true),
                ..Default::default()
            }),
        );
        assert!(compiler.cue_contributor_is_active(&contributor, &ir));
    }

    #[test]
    fn test_contributor_always_false_inactive() {
        let project = Project::new("test");
        let compiler = Compiler::new(project);
        let ir = test_ir();

        // always: false explicitly disables the contributor
        let contributor = test_contributor(
            "test",
            Some(ActivationCondition {
                always: Some(false),
                ..Default::default()
            }),
        );
        assert!(!compiler.cue_contributor_is_active(&contributor, &ir));
    }

    #[test]
    fn test_contributor_runtime_type_matches_nix() {
        use cuenv_core::manifest::{NixRuntime, Runtime};

        let mut project = Project::new("test");
        project.runtime = Some(Runtime::Nix(NixRuntime::default()));

        let compiler = Compiler::new(project);
        let ir = test_ir();

        let contributor = test_contributor(
            "nix",
            Some(ActivationCondition {
                runtime_type: vec!["nix".to_string()],
                ..Default::default()
            }),
        );
        assert!(compiler.cue_contributor_is_active(&contributor, &ir));
    }

    #[test]
    fn test_contributor_runtime_type_no_match() {
        use cuenv_core::manifest::{NixRuntime, Runtime};

        let mut project = Project::new("test");
        project.runtime = Some(Runtime::Nix(NixRuntime::default()));

        let compiler = Compiler::new(project);
        let ir = test_ir();

        // Project has Nix runtime, but condition requires "devenv"
        let contributor = test_contributor(
            "devenv-only",
            Some(ActivationCondition {
                runtime_type: vec!["devenv".to_string()],
                ..Default::default()
            }),
        );
        assert!(!compiler.cue_contributor_is_active(&contributor, &ir));
    }

    #[test]
    fn test_contributor_runtime_type_no_runtime_set() {
        let project = Project::new("test");
        let compiler = Compiler::new(project);
        let ir = test_ir();

        // No runtime set but condition requires runtime type
        let contributor = test_contributor(
            "needs-nix",
            Some(ActivationCondition {
                runtime_type: vec!["nix".to_string()],
                ..Default::default()
            }),
        );
        assert!(!compiler.cue_contributor_is_active(&contributor, &ir));
    }

    #[test]
    fn test_contributor_cuenv_source_matches() {
        use cuenv_core::ci::CI;
        use cuenv_core::config::{CIConfig, CuenvConfig, CuenvSource};
        use std::collections::BTreeMap;

        let mut project = Project::new("test");
        project.config = Some(cuenv_core::config::Config::default());
        project.ci = Some(CI {
            pipelines: BTreeMap::new(),
            provider: None,
            contributors: vec![],
        });
        // Set cuenv source to "git"
        if let Some(ref mut config) = project.config {
            config.ci = Some(CIConfig {
                cuenv: Some(CuenvConfig {
                    source: CuenvSource::Git,
                    ..Default::default()
                }),
            });
        }

        let compiler = Compiler::new(project);
        let ir = test_ir();

        let contributor = test_contributor(
            "cuenv-git",
            Some(ActivationCondition {
                cuenv_source: vec!["git".to_string()],
                ..Default::default()
            }),
        );
        assert!(compiler.cue_contributor_is_active(&contributor, &ir));
    }

    #[test]
    fn test_contributor_multiple_conditions_and_logic() {
        use cuenv_core::manifest::{NixRuntime, Runtime};

        let mut project = Project::new("test");
        project.runtime = Some(Runtime::Nix(NixRuntime::default()));

        let compiler = Compiler::new(project);
        let ir = test_ir();

        // Condition requires nix runtime AND devenv source (which doesn't match)
        let contributor = test_contributor(
            "multi-condition",
            Some(ActivationCondition {
                runtime_type: vec!["nix".to_string()],
                cuenv_source: vec!["nix".to_string()], // default is "release", not "nix"
                ..Default::default()
            }),
        );
        // Runtime matches but cuenv_source doesn't (default is "release")
        assert!(!compiler.cue_contributor_is_active(&contributor, &ir));
    }

    // =========================================================================
    // Contributor Task Conversion Tests
    // =========================================================================

    #[test]
    fn test_contributor_task_to_ir_command() {
        let contributor_task = ContributorTask {
            id: "test-task".to_string(),
            label: Some("Test Task".to_string()),
            description: None,
            command: Some("echo".to_string()),
            args: vec!["hello".to_string()],
            script: None,
            shell: false,
            env: HashMap::default(),
            secrets: HashMap::default(),
            inputs: vec![],
            outputs: vec![],
            hermetic: false,
            depends_on: vec![],
            priority: 10,
            condition: None,
            provider: None,
        };

        let ir_task = Compiler::contributor_task_to_ir(&contributor_task, "github");

        assert_eq!(ir_task.id, "cuenv:contributor:test-task");
        assert_eq!(ir_task.command, vec!["echo", "hello"]);
        assert!(!ir_task.shell);
        assert_eq!(ir_task.priority, Some(10));
        assert_eq!(ir_task.phase, Some(BuildStage::Setup)); // priority 10 = Setup
    }

    #[test]
    fn test_contributor_task_to_ir_script() {
        let contributor_task = ContributorTask {
            id: "script-task".to_string(),
            label: None,
            description: None,
            command: None,
            args: vec![],
            script: Some("echo line1\necho line2".to_string()),
            shell: true,
            env: HashMap::default(),
            secrets: HashMap::default(),
            inputs: vec![],
            outputs: vec![],
            hermetic: false,
            depends_on: vec!["other".to_string()],
            priority: 5,
            condition: None,
            provider: None,
        };

        let ir_task = Compiler::contributor_task_to_ir(&contributor_task, "github");

        assert_eq!(ir_task.id, "cuenv:contributor:script-task");
        assert_eq!(ir_task.command, vec!["echo line1\necho line2"]);
        assert!(ir_task.shell);
        assert_eq!(ir_task.depends_on, vec!["cuenv:contributor:other"]);
        assert_eq!(ir_task.priority, Some(5));
        assert_eq!(ir_task.phase, Some(BuildStage::Bootstrap)); // priority 5 = Bootstrap
    }

    #[test]
    fn test_contributor_task_to_ir_github_action() {
        use cuenv_core::ci::{GitHubActionConfig, TaskProviderConfig};

        let mut inputs = std::collections::HashMap::new();
        inputs.insert(
            "extra-conf".to_string(),
            serde_json::Value::String("accept-flake-config = true".to_string()),
        );

        let contributor_task = ContributorTask {
            id: "nix.install".to_string(),
            label: Some("Install Nix".to_string()),
            description: None,
            command: None,
            args: vec![],
            script: None,
            shell: false,
            env: HashMap::default(),
            secrets: HashMap::default(),
            inputs: vec![],
            outputs: vec![],
            hermetic: false,
            depends_on: vec![],
            priority: 0,
            condition: None,
            provider: Some(TaskProviderConfig {
                github: Some(GitHubActionConfig {
                    uses: "DeterminateSystems/nix-installer-action@v16".to_string(),
                    inputs,
                }),
            }),
        };

        let ir_task = Compiler::contributor_task_to_ir(&contributor_task, "nix");

        assert_eq!(ir_task.id, "cuenv:contributor:nix.install");
        assert!(ir_task.command.is_empty()); // No command, uses action
        assert!(ir_task.provider_hints.is_some());
        assert_eq!(ir_task.phase, Some(BuildStage::Bootstrap)); // priority 0 = Bootstrap

        // Verify the GitHub action is in provider_hints
        let hints = ir_task.provider_hints.as_ref().unwrap();
        let github_action = hints.get("github_action").unwrap();
        assert_eq!(
            github_action.get("uses").and_then(|v| v.as_str()),
            Some("DeterminateSystems/nix-installer-action@v16")
        );
    }

    #[test]
    fn test_contributor_task_to_ir_secrets() {
        use cuenv_core::ci::SecretRefConfig;

        let mut secrets = std::collections::HashMap::new();
        secrets.insert(
            "SIMPLE_SECRET".to_string(),
            SecretRef::Simple("SECRET_NAME".to_string()),
        );
        secrets.insert(
            "DETAILED_SECRET".to_string(),
            SecretRef::Detailed(SecretRefConfig {
                source: "DETAILED_SOURCE".to_string(),
                cache_key: true,
            }),
        );

        let contributor_task = ContributorTask {
            id: "secrets-task".to_string(),
            label: None,
            description: None,
            command: Some("echo".to_string()),
            args: vec!["test".to_string()],
            script: None,
            shell: false,
            env: HashMap::default(),
            secrets,
            inputs: vec![],
            outputs: vec![],
            hermetic: false,
            depends_on: vec![],
            priority: 10,
            condition: None,
            provider: None,
        };

        let ir_task = Compiler::contributor_task_to_ir(&contributor_task, "github");

        assert_eq!(ir_task.secrets.len(), 2);
        assert_eq!(ir_task.phase, Some(BuildStage::Setup));

        // Check simple secret conversion
        let simple = ir_task.secrets.get("SIMPLE_SECRET").unwrap();
        assert_eq!(simple.source, "SECRET_NAME");
        assert!(!simple.cache_key);

        // Check detailed secret conversion
        let detailed = ir_task.secrets.get("DETAILED_SECRET").unwrap();
        assert_eq!(detailed.source, "DETAILED_SOURCE");
        assert!(detailed.cache_key);
    }

    #[test]
    fn test_contributor_task_to_ir_env_vars() {
        let mut env = std::collections::HashMap::new();
        env.insert("VAR1".to_string(), "value1".to_string());
        env.insert("VAR2".to_string(), "value2".to_string());

        let contributor_task = ContributorTask {
            id: "env-task".to_string(),
            label: None,
            description: None,
            command: Some("printenv".to_string()),
            args: vec![],
            script: None,
            shell: false,
            env,
            secrets: HashMap::default(),
            inputs: vec![],
            outputs: vec![],
            hermetic: false,
            depends_on: vec![],
            priority: 10,
            condition: None,
            provider: None,
        };

        let ir_task = Compiler::contributor_task_to_ir(&contributor_task, "github");

        assert_eq!(ir_task.env.len(), 2);
        assert_eq!(ir_task.env.get("VAR1"), Some(&"value1".to_string()));
        assert_eq!(ir_task.env.get("VAR2"), Some(&"value2".to_string()));
        assert_eq!(ir_task.phase, Some(BuildStage::Setup));
    }

    #[test]
    fn test_contributor_task_to_ir_command_with_args() {
        let contributor_task = ContributorTask {
            id: "bun.workspace.install".to_string(),
            label: Some("Install Bun Dependencies".to_string()),
            description: None,
            command: Some("bun".to_string()),
            args: vec!["install".to_string(), "--frozen-lockfile".to_string()],
            script: None,
            shell: false,
            env: HashMap::default(),
            secrets: HashMap::default(),
            inputs: vec!["package.json".to_string(), "bun.lock".to_string()],
            outputs: vec![],
            hermetic: false,
            depends_on: vec![],
            priority: 10,
            condition: None,
            provider: None,
        };

        let ir_task = Compiler::contributor_task_to_ir(&contributor_task, "bun.workspace");

        assert_eq!(ir_task.id, "cuenv:contributor:bun.workspace.install");
        assert_eq!(ir_task.command, vec!["bun", "install", "--frozen-lockfile"]);
        assert!(!ir_task.shell);
        assert_eq!(ir_task.phase, Some(BuildStage::Setup));
        assert_eq!(ir_task.inputs, vec!["package.json", "bun.lock"]);
    }

    #[test]
    fn test_derive_stage_from_priority_bootstrap() {
        // Priority 0-9 = Bootstrap
        assert_eq!(
            Compiler::derive_stage_from_priority(0, None),
            BuildStage::Bootstrap
        );
        assert_eq!(
            Compiler::derive_stage_from_priority(5, None),
            BuildStage::Bootstrap
        );
        assert_eq!(
            Compiler::derive_stage_from_priority(9, None),
            BuildStage::Bootstrap
        );
    }

    #[test]
    fn test_derive_stage_from_priority_setup() {
        // Priority 10-49 = Setup
        assert_eq!(
            Compiler::derive_stage_from_priority(10, None),
            BuildStage::Setup
        );
        assert_eq!(
            Compiler::derive_stage_from_priority(25, None),
            BuildStage::Setup
        );
        assert_eq!(
            Compiler::derive_stage_from_priority(49, None),
            BuildStage::Setup
        );
    }

    #[test]
    fn test_derive_stage_from_priority_success() {
        // Priority 50+ = Success
        assert_eq!(
            Compiler::derive_stage_from_priority(50, None),
            BuildStage::Success
        );
        assert_eq!(
            Compiler::derive_stage_from_priority(100, None),
            BuildStage::Success
        );
    }

    #[test]
    fn test_derive_stage_from_priority_failure_condition() {
        // on_failure condition = Failure regardless of priority
        assert_eq!(
            Compiler::derive_stage_from_priority(0, Some(CueTaskCondition::OnFailure)),
            BuildStage::Failure
        );
        assert_eq!(
            Compiler::derive_stage_from_priority(50, Some(CueTaskCondition::OnFailure)),
            BuildStage::Failure
        );
    }

    // Tests for cue_task_condition_to_ir
    #[test]
    fn test_cue_task_condition_to_ir_on_success() {
        let result = Compiler::cue_task_condition_to_ir(CueTaskCondition::OnSuccess);
        assert_eq!(result, TaskCondition::OnSuccess);
    }

    #[test]
    fn test_cue_task_condition_to_ir_on_failure() {
        let result = Compiler::cue_task_condition_to_ir(CueTaskCondition::OnFailure);
        assert_eq!(result, TaskCondition::OnFailure);
    }

    #[test]
    fn test_cue_task_condition_to_ir_always() {
        let result = Compiler::cue_task_condition_to_ir(CueTaskCondition::Always);
        assert_eq!(result, TaskCondition::Always);
    }

    // =========================================================================
    // Path Derivation Tests
    // =========================================================================

    use cuenv_core::ci::{PipelineCondition, PipelineTask, StringOrVec};
    use cuenv_core::tasks::{Input, ParallelGroup};

    #[test]
    fn test_derive_paths_from_task_group() {
        // Create a task group (like "check" with nested tasks "lint", "test", etc.)
        let mut project = Project::new("test-project");

        let mut group_tasks = HashMap::new();
        group_tasks.insert(
            "lint".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo".to_string(),
                args: vec!["clippy".to_string()],
                inputs: vec![
                    Input::Path("Cargo.toml".to_string()),
                    Input::Path("crates/**".to_string()),
                ],
                ..Default::default()
            })),
        );
        group_tasks.insert(
            "test".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo".to_string(),
                args: vec!["test".to_string()],
                inputs: vec![
                    Input::Path("Cargo.toml".to_string()),
                    Input::Path("crates/**".to_string()),
                    Input::Path("tests/**".to_string()),
                ],
                ..Default::default()
            })),
        );

        project.tasks.insert(
            "check".to_string(),
            TaskDefinition::Group(TaskGroup::Parallel(ParallelGroup {
                tasks: group_tasks,
                depends_on: vec![],
            })),
        );

        let pipeline = Pipeline {
            mode: PipelineMode::default(),
            environment: None,
            tasks: vec![PipelineTask::Simple("check".to_string())],
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
            pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
            ..Default::default()
        });

        // Root project (no project_path prefix)
        let options = CompilerOptions {
            pipeline_name: Some("default".to_string()),
            pipeline: Some(pipeline),
            project_path: None,
            ..Default::default()
        };

        let compiler = Compiler::with_options(project, options);
        let ir = compiler.compile().unwrap();

        let trigger = ir.pipeline.trigger.expect("should have trigger");

        // Should collect inputs from all nested tasks in the group
        assert!(
            trigger.paths.contains(&"Cargo.toml".to_string()),
            "Should contain Cargo.toml from group tasks. Paths: {:?}",
            trigger.paths
        );
        assert!(
            trigger.paths.contains(&"crates/**".to_string()),
            "Should contain crates/** from group tasks. Paths: {:?}",
            trigger.paths
        );
        assert!(
            trigger.paths.contains(&"tests/**".to_string()),
            "Should contain tests/** from group tasks. Paths: {:?}",
            trigger.paths
        );
        // Should NOT fallback to ** since we have inputs
        assert!(
            !trigger.paths.contains(&"**".to_string()),
            "Should not fallback to ** when task group has inputs. Paths: {:?}",
            trigger.paths
        );
    }

    #[test]
    fn test_derive_paths_root_project_no_dot_prefix() {
        // When project_path is "." (root), paths should not have "./" prefix
        let mut project = Project::new("test-project");

        project.tasks.insert(
            "build".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo".to_string(),
                args: vec!["build".to_string()],
                inputs: vec![Input::Path("src/**".to_string())],
                ..Default::default()
            })),
        );

        let pipeline = Pipeline {
            mode: PipelineMode::default(),
            environment: None,
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
            pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
            ..Default::default()
        });

        // project_path = "." (root project, as set by sync command)
        let options = CompilerOptions {
            pipeline_name: Some("default".to_string()),
            pipeline: Some(pipeline),
            project_path: Some(".".to_string()),
            ..Default::default()
        };

        let compiler = Compiler::with_options(project, options);
        let ir = compiler.compile().unwrap();

        let trigger = ir.pipeline.trigger.expect("should have trigger");

        // Paths should NOT have "./" prefix - GitHub Actions doesn't handle it correctly
        assert!(
            trigger.paths.contains(&"src/**".to_string()),
            "Should contain src/** without ./ prefix. Paths: {:?}",
            trigger.paths
        );
        assert!(
            !trigger.paths.iter().any(|p| p.starts_with("./")),
            "No path should have ./ prefix. Paths: {:?}",
            trigger.paths
        );
        assert!(
            trigger.paths.contains(&"env.cue".to_string()),
            "Should contain env.cue without ./ prefix. Paths: {:?}",
            trigger.paths
        );
    }

    #[test]
    fn test_derive_paths_subproject_has_prefix() {
        // When project_path is "projects/api", paths should be prefixed
        let mut project = Project::new("test-project");

        project.tasks.insert(
            "build".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo".to_string(),
                args: vec!["build".to_string()],
                inputs: vec![Input::Path("src/**".to_string())],
                ..Default::default()
            })),
        );

        let pipeline = Pipeline {
            mode: PipelineMode::default(),
            environment: None,
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
            pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
            ..Default::default()
        });

        // Subproject path
        let options = CompilerOptions {
            pipeline_name: Some("default".to_string()),
            pipeline: Some(pipeline),
            project_path: Some("projects/api".to_string()),
            ..Default::default()
        };

        let compiler = Compiler::with_options(project, options);
        let ir = compiler.compile().unwrap();

        let trigger = ir.pipeline.trigger.expect("should have trigger");

        // Paths should have the project prefix
        assert!(
            trigger.paths.contains(&"projects/api/src/**".to_string()),
            "Should contain prefixed path. Paths: {:?}",
            trigger.paths
        );
        assert!(
            trigger.paths.contains(&"projects/api/env.cue".to_string()),
            "Should contain prefixed env.cue. Paths: {:?}",
            trigger.paths
        );
    }
}
