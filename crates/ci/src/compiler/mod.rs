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

mod contributors;
pub mod digest;

use crate::flake::{FlakeLockAnalyzer, FlakeLockError, PurityAnalysis};
use crate::ir::{
    ArtifactDownload, CachePolicy, IntermediateRepresentation, IrValidator, ManualTriggerConfig,
    OutputDeclaration, OutputType, PurityMode, Runtime, SecretConfig, Task as IrTask,
    TriggerCondition, WorkflowDispatchInputDef,
};
use cuenv_core::ci::{CI, ManualTrigger, Pipeline, PipelineTask};
use cuenv_core::manifest::Project;
use cuenv_core::tasks::{Task, TaskGroup, TaskNode};
use digest::DigestBuilder;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
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

        // Find the task node
        let Some(task_node) = self.find_task_node(task_name) else {
            return Err(CompilerError::TaskNotFound(task_name.to_string()));
        };

        // Compile just this task node
        self.compile_task_node(task_name, task_node, &mut ir)?;

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

        TriggerCondition {
            branches,
            pull_request,
            scheduled,
            release,
            manual,
            paths,
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

        // Add task inputs, canonicalized into repo-relative trigger filters.
        //
        // GitHub Actions path filters are glob-only — a bare `server/src`
        // matches only the literal path, not its descendants. cuenv's own
        // affected matching (`cuenv_core::matches_pattern`) treats non-glob
        // patterns as prefixes, so to keep the two in agreement we also emit
        // `<path>/**` for any user input that does not already contain glob
        // metacharacters. This costs one extra entry per simple-path input
        // and covers both "input is a file" and "input is a directory".
        for input in &task_inputs {
            match repo_relative_trigger_path(self.options.project_path.as_deref(), input) {
                Some(path) => {
                    if is_simple_path_pattern(input) {
                        paths.insert(format!("{path}/**"));
                    }
                    paths.insert(path);
                }
                None => {
                    tracing::warn!(
                        project_path = self.options.project_path.as_deref().unwrap_or("."),
                        input = input.as_str(),
                        "Skipping task input that escapes the repository root or is absolute; \
                         it will not contribute to derived GitHub path filters",
                    );
                }
            }
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
        if let Some(path) =
            repo_relative_trigger_path(self.options.project_path.as_deref(), "env.cue")
        {
            paths.insert(path);
        }
        if let Some(path) =
            repo_relative_trigger_path(self.options.project_path.as_deref(), "schema/**")
        {
            paths.insert(path);
        }
        // cue.mod is always at module root, not prefixed with project_path
        paths.insert("cue.mod/**".to_string());

        // Add workspace member dependency paths
        self.add_workspace_dependency_paths(&mut paths);

        // Sort for deterministic output
        let mut result: Vec<_> = paths.into_iter().collect();
        result.sort();
        result
    }

    /// Adds paths for workspace member dependencies (direct and transitive).
    ///
    /// If the current project is a member of a workspace (JS/npm, pnpm, or Cargo),
    /// this finds all other workspace members that this project depends on and
    /// adds their paths to the trigger paths.
    ///
    /// # Supported Workspace Types
    /// - npm/yarn workspaces (package.json)
    /// - pnpm workspaces (pnpm-workspace.yaml)
    /// - Cargo workspaces (Cargo.toml)
    ///
    /// # Testing
    /// Core dependency resolution logic is tested in `cuenv_workspaces::Workspace`.
    /// See `crates/workspaces/src/core/types.rs` for unit tests covering direct,
    /// transitive, and circular dependency resolution.
    fn add_workspace_dependency_paths(&self, paths: &mut HashSet<String>) {
        use cuenv_workspaces::{
            CargoTomlDiscovery, PackageJsonDiscovery, PnpmWorkspaceDiscovery, Workspace,
            WorkspaceDiscovery,
        };

        let Some(ref project_path) = self.options.project_path else {
            return; // Root project, no workspace dependency resolution needed
        };

        if project_path == "." {
            return; // Root project
        }

        let module_root = self
            .options
            .module_root
            .clone()
            .or_else(|| self.options.project_root.clone())
            .unwrap_or_else(|| PathBuf::from("."));

        // Try all workspace discovery methods
        let workspace: Option<Workspace> = PackageJsonDiscovery
            .discover(&module_root)
            .ok()
            .or_else(|| PnpmWorkspaceDiscovery.discover(&module_root).ok())
            .or_else(|| CargoTomlDiscovery.discover(&module_root).ok());

        let Some(workspace) = workspace else {
            return; // Not in a supported workspace
        };

        // Find current project as a workspace member by path
        let project_path_buf = Path::new(project_path);
        let Some(current_member) = workspace.find_member_by_path(project_path_buf) else {
            return; // Current project not found as workspace member
        };

        // Resolve transitive workspace dependencies
        let dep_paths = workspace.resolve_workspace_dependency_paths(&current_member.name);

        // Add each dependency's path as a glob pattern
        for dep_path in dep_paths {
            let mut pattern = dep_path.clone();
            pattern.push("**");
            paths.insert(pattern.to_string_lossy().into_owned());
        }
    }

    /// Recursively collect task inputs including dependencies
    fn collect_task_inputs(&self, task_name: &str, paths: &mut HashSet<String>) {
        if let Some(node) = self.find_task_node(task_name) {
            self.collect_inputs_from_node(node, paths);
        }
    }

    /// Collect inputs from a task node (handles tasks, groups, and lists)
    fn collect_inputs_from_node(&self, node: &TaskNode, paths: &mut HashSet<String>) {
        match node {
            TaskNode::Task(task) => {
                // Add direct inputs
                for input in task.iter_path_inputs() {
                    paths.insert(input.clone());
                }
                // Recurse into dependencies
                for dep in &task.depends_on {
                    self.collect_task_inputs(dep.task_name(), paths);
                }
            }
            TaskNode::Group(group) => {
                for child_node in group.children.values() {
                    self.collect_inputs_from_node(child_node, paths);
                }
            }
            TaskNode::Sequence(steps) => {
                for child_node in steps {
                    self.collect_inputs_from_node(child_node, paths);
                }
            }
        }
    }

    /// Find a task node by name (handles dotted paths for nested tasks)
    /// Returns the TaskNode which can be a Task, Group, or List
    fn find_task_node(&self, name: &str) -> Option<&TaskNode> {
        let parts: Vec<&str> = name.split('.').collect();
        let mut current_tasks = &self.project.tasks;

        for (i, part) in parts.iter().enumerate() {
            match current_tasks.get(*part) {
                Some(node) if i == parts.len() - 1 => {
                    return Some(node);
                }
                Some(TaskNode::Group(group)) => {
                    current_tasks = &group.children;
                }
                _ => return None,
            }
        }
        None
    }

    /// Find a leaf task by name (handles dotted paths for nested tasks)
    fn find_task(&self, name: &str) -> Option<&Task> {
        match self.find_task_node(name) {
            Some(TaskNode::Task(task)) => Some(task),
            _ => None,
        }
    }

    /// Expand a dependency name to its leaf task names.
    ///
    /// If the dependency refers to:
    /// - A leaf Task: returns `[dep_name]`
    /// - A TaskGroup: returns all leaf children (recursively), sorted alphabetically
    /// - A Sequence: returns all leaf tasks in the sequence (recursively)
    /// - Non-existent: tries sibling resolution, then returns as-is for validation
    ///
    /// # Arguments
    /// * `dep_name` - The dependency name (may be simple like "build" or qualified like "docs.build")
    /// * `current_task_id` - The ID of the task that has this dependency (e.g., "docs.deploy")
    fn expand_dependency_to_leaf_tasks(
        &self,
        dep_name: &str,
        current_task_id: &str,
    ) -> Vec<String> {
        // First, try direct lookup
        if let Some(node) = self.find_task_node(dep_name) {
            let mut result = Vec::new();
            Self::collect_leaf_task_names(dep_name, node, &mut result);
            result.sort();
            return result;
        }

        // Not found directly - try sibling resolution
        // If current task is "docs.deploy" and dep is "build", try "docs.build"
        if let Some(parent_path) = current_task_id.rsplit_once('.').map(|(parent, _)| parent) {
            let sibling_path = format!("{parent_path}.{dep_name}");
            if let Some(node) = self.find_task_node(&sibling_path) {
                let mut result = Vec::new();
                Self::collect_leaf_task_names(&sibling_path, node, &mut result);
                result.sort();
                return result;
            }
        }

        // Task not found - return as-is and let validation report the error
        vec![dep_name.to_string()]
    }

    /// Recursively collect all leaf task names from a TaskNode.
    /// This is a helper function, not a method, since it doesn't need compiler state.
    fn collect_leaf_task_names(prefix: &str, node: &TaskNode, result: &mut Vec<String>) {
        match node {
            TaskNode::Task(_) => {
                result.push(prefix.to_string());
            }
            TaskNode::Group(group) => {
                for (child_name, child_node) in &group.children {
                    Self::collect_leaf_task_names(
                        &format!("{prefix}.{child_name}"),
                        child_node,
                        result,
                    );
                }
            }
            TaskNode::Sequence(steps) => {
                for (idx, child_node) in steps.iter().enumerate() {
                    Self::collect_leaf_task_names(&format!("{prefix}.{idx}"), child_node, result);
                }
            }
        }
    }

    /// Compile task nodes into IR tasks
    fn compile_tasks(
        &self,
        tasks: &HashMap<String, TaskNode>,
        ir: &mut IntermediateRepresentation,
    ) -> Result<(), CompilerError> {
        // Sort keys for deterministic output
        let mut sorted_keys: Vec<_> = tasks.keys().collect();
        sorted_keys.sort();
        for name in sorted_keys {
            let task_node = &tasks[name];
            self.compile_task_node(name, task_node, ir)?;
        }
        Ok(())
    }

    /// Compile a task node (handles tasks, groups, and lists)
    fn compile_task_node(
        &self,
        name: &str,
        node: &TaskNode,
        ir: &mut IntermediateRepresentation,
    ) -> Result<(), CompilerError> {
        match node {
            TaskNode::Task(task) => {
                let ir_task = self.compile_single_task(name, task)?;
                ir.tasks.push(ir_task);
            }
            TaskNode::Group(group) => {
                self.compile_task_group(name, group, ir)?;
            }
            TaskNode::Sequence(steps) => {
                self.compile_task_sequence(name, steps, ir)?;
            }
        }
        Ok(())
    }

    /// Compile a task group (parallel execution)
    fn compile_task_group(
        &self,
        prefix: &str,
        group: &TaskGroup,
        ir: &mut IntermediateRepresentation,
    ) -> Result<(), CompilerError> {
        // Sort keys for deterministic output
        let mut sorted_keys: Vec<_> = group.children.keys().collect();
        sorted_keys.sort();
        for name in sorted_keys {
            let child_node = &group.children[name];
            let task_name = format!("{prefix}.{name}");
            self.compile_task_node(&task_name, child_node, ir)?;
        }
        Ok(())
    }

    /// Compile a task sequence (sequential execution)
    fn compile_task_sequence(
        &self,
        prefix: &str,
        steps: &[TaskNode],
        ir: &mut IntermediateRepresentation,
    ) -> Result<(), CompilerError> {
        for (idx, child_node) in steps.iter().enumerate() {
            let task_name = format!("{prefix}.{idx}");
            self.compile_task_node(&task_name, child_node, ir)?;
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
            depends_on: task
                .depends_on
                .iter()
                .flat_map(|d| self.expand_dependency_to_leaf_tasks(d.task_name(), id))
                .collect(),
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
}

/// Convert a project-relative input glob into a repo-relative trigger path.
///
/// GitHub Actions path filters compare changed file names as repo-relative
/// strings. They do not normalize `server/../flake.nix`, so derived trigger
/// paths must be cleaned before emission.
fn repo_relative_trigger_path(project_path: Option<&str>, input: &str) -> Option<String> {
    let mut path = PathBuf::new();

    if let Some(project_path) = project_path.filter(|p| !p.is_empty() && *p != ".")
        && !push_relative_components(&mut path, Path::new(project_path))
    {
        return None;
    }

    if !push_relative_components(&mut path, Path::new(input)) {
        return None;
    }

    let rendered = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");

    if rendered.is_empty() {
        None
    } else {
        Some(rendered)
    }
}

/// True when `input` contains none of the glob metacharacters that
/// `cuenv_core::affected::matches_pattern` uses to switch between glob
/// matching and prefix matching. Kept in sync with that function so derived
/// GitHub trigger paths stay aligned with local affected detection.
fn is_simple_path_pattern(input: &str) -> bool {
    !input.contains('*') && !input.contains('?') && !input.contains('[')
}

fn push_relative_components(path: &mut PathBuf, input: &Path) -> bool {
    for component in input.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => path.push(part),
            Component::ParentDir => {
                if !path.pop() {
                    return false;
                }
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }

    true
}

#[cfg(test)]
#[path = "compiler_tests.rs"]
mod tests;
