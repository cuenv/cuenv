//! IR v1.5 Schema Types
//!
//! JSON schema for the intermediate representation used by the CI pipeline compiler.
//!
//! ## Version History
//! - v1.5: Unified task model - phase tasks have `phase` field instead of separate `stages`
//! - v1.4: Added `stages` field for provider-injected setup tasks (deprecated in v1.5)
//! - v1.3: Initial stable version

use cuenv_core::ci::{PipelineMode, PipelineTask};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// IR version identifier
pub const IR_VERSION: &str = "1.5";

/// Root IR document
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IntermediateRepresentation {
    /// IR version (always "1.5")
    pub version: String,

    /// Pipeline metadata
    pub pipeline: PipelineMetadata,

    /// Runtime environment definitions
    #[serde(default)]
    pub runtimes: Vec<Runtime>,

    /// Task definitions (includes both regular tasks and phase tasks)
    pub tasks: Vec<Task>,
}

impl IntermediateRepresentation {
    /// Create a new IR document
    pub fn new(pipeline_name: impl Into<String>) -> Self {
        Self {
            version: IR_VERSION.to_string(),
            pipeline: PipelineMetadata {
                name: pipeline_name.into(),
                mode: PipelineMode::default(),
                environment: None,
                requires_onepassword: false,
                project_name: None,
                project_path: None,
                trigger: None,
                pipeline_tasks: Vec::new(),
                pipeline_task_defs: Vec::new(),
            },
            runtimes: Vec::new(),
            tasks: Vec::new(),
        }
    }

    /// Get all tasks belonging to a specific phase (unified model).
    ///
    /// Returns an iterator over tasks that have `phase` set to the given stage.
    /// These are tasks contributed by CUE contributors.
    pub fn phase_tasks(&self, stage: BuildStage) -> impl Iterator<Item = &Task> {
        self.tasks.iter().filter(move |t| t.phase == Some(stage))
    }

    /// Get all regular tasks (not phase tasks).
    ///
    /// Returns an iterator over tasks that have no phase set.
    /// These are the main pipeline tasks defined in the project.
    pub fn regular_tasks(&self) -> impl Iterator<Item = &Task> {
        self.tasks.iter().filter(|t| t.phase.is_none())
    }

    /// Get phase tasks sorted by priority (lower = earlier).
    ///
    /// Collects phase tasks into a Vec and sorts them by priority.
    /// Uses priority 10 as default if not specified.
    #[must_use]
    pub fn sorted_phase_tasks(&self, stage: BuildStage) -> Vec<&Task> {
        let mut tasks: Vec<_> = self.phase_tasks(stage).collect();
        tasks.sort_by_key(|t| t.priority.unwrap_or(10));
        tasks
    }
}

/// Pipeline metadata and trigger configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PipelineMetadata {
    /// Pipeline name
    pub name: String,

    /// Generation mode (thin or expanded)
    #[serde(default)]
    pub mode: PipelineMode,

    /// Environment for secret resolution (e.g., "production")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,

    /// Whether this pipeline requires 1Password for secret resolution
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub requires_onepassword: bool,

    /// Project name (for monorepo prefixing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,

    /// Relative path to project directory (for working-directory in monorepos)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,

    /// Trigger conditions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<TriggerCondition>,

    /// Task IDs that this pipeline runs (for contributor filtering)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pipeline_tasks: Vec<String>,

    /// Full pipeline task definitions with matrix configurations preserved
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pipeline_task_defs: Vec<PipelineTask>,
}

/// Trigger conditions for pipeline execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub struct TriggerCondition {
    /// Branch patterns to trigger on
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<String>,

    /// Enable pull request triggers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<bool>,

    /// Cron expressions for scheduled runs
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scheduled: Vec<String>,

    /// Release event types (e.g., `["published"]`)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub release: Vec<String>,

    /// Manual trigger configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual: Option<ManualTriggerConfig>,

    /// Path patterns derived from task inputs (triggers on these paths)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

/// Manual trigger (`workflow_dispatch`) configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ManualTriggerConfig {
    /// Whether manual trigger is enabled
    pub enabled: bool,

    /// Input definitions for `workflow_dispatch`
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, WorkflowDispatchInputDef>,
}

/// Workflow dispatch input definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowDispatchInputDef {
    /// Human-readable description
    pub description: String,

    /// Whether the input is required
    #[serde(default)]
    pub required: bool,

    /// Default value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,

    /// Input type (string, boolean, choice, environment)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,

    /// Options for choice-type inputs
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

/// Runtime environment definition (Nix flake-based)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Runtime {
    /// Unique runtime identifier
    pub id: String,

    /// Nix flake reference (e.g., "github:NixOS/nixpkgs/nixos-unstable")
    pub flake: String,

    /// Flake output path (e.g., "devShells.x86_64-linux.default")
    pub output: String,

    /// System architecture (e.g., "x86_64-linux", "aarch64-darwin")
    pub system: String,

    /// Runtime digest for caching (computed from flake.lock + output)
    pub digest: String,

    /// Purity enforcement mode
    #[serde(default)]
    pub purity: PurityMode,
}

/// Purity enforcement for Nix flakes
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PurityMode {
    /// Reject unlocked flakes (strict mode)
    Strict,

    /// Warn on unlocked flakes, inject UUID into digest (default)
    #[default]
    Warning,

    /// Allow manual input pinning at compile time
    Override,
}

/// Task definition in the IR
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Task {
    /// Unique task identifier
    pub id: String,

    /// Runtime environment ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,

    /// Command to execute (array form for direct execve)
    pub command: Vec<String>,

    /// Shell execution mode (false = direct execve, true = wrap in /bin/sh -c)
    #[serde(default)]
    pub shell: bool,

    /// Environment variables
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,

    /// Secret configurations
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, SecretConfig>,

    /// Resource requirements (for scheduling)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceRequirements>,

    /// Concurrency group for serialized execution
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency_group: Option<String>,

    /// Input file globs (expanded at compile time)
    #[serde(default)]
    pub inputs: Vec<String>,

    /// Output declarations
    #[serde(default)]
    pub outputs: Vec<OutputDeclaration>,

    /// Task dependencies (must complete before this task runs)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Cache policy
    #[serde(default)]
    pub cache_policy: CachePolicy,

    /// Deployment flag (if true, `cache_policy` is forced to disabled)
    #[serde(default)]
    pub deployment: bool,

    /// Manual approval required before execution
    #[serde(default)]
    pub manual_approval: bool,

    /// Matrix configuration for parallel job expansion
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matrix: Option<MatrixConfig>,

    /// Artifacts to download before running this task
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_downloads: Vec<ArtifactDownload>,

    /// Parameters to pass to the task command
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,

    // ==========================================================================
    // Phase task fields (for unified task model)
    // ==========================================================================
    /// Phase this task belongs to (None = regular task, Some = phase task)
    ///
    /// Phase tasks are contributed by CUE contributors and run at specific
    /// lifecycle points: bootstrap, setup, success, or failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<BuildStage>,

    /// Human-readable label for display (primarily for phase tasks)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Priority within phase (lower = earlier, default 10)
    ///
    /// Only meaningful for phase tasks. Used for ordering when multiple
    /// contributors add tasks to the same phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,

    /// Contributor that added this task (e.g., "nix", "codecov")
    ///
    /// Set when this task was contributed by a CUE contributor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contributor: Option<String>,

    /// Execution condition for phase tasks
    ///
    /// Determines when the task runs relative to other task outcomes:
    /// - `OnSuccess`: Run only if all prior tasks succeeded
    /// - `OnFailure`: Run only if any prior task failed
    /// - `Always`: Run regardless of prior task outcomes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<TaskCondition>,

    /// Provider-specific hints (e.g., GitHub Action specs)
    ///
    /// Opaque JSON value that provider-specific emitters can interpret.
    /// For GitHub, may contain `{ "github_action": { "uses": "...", "with": {...} } }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_hints: Option<serde_json::Value>,
}

impl Task {
    /// Get the display label for this task, falling back to the ID.
    ///
    /// Used by renderers when generating step names in CI workflows.
    #[must_use]
    pub fn label(&self) -> String {
        self.label.clone().unwrap_or_else(|| self.id.clone())
    }

    /// Get the command as a single string (for shell execution).
    ///
    /// Joins the command array with spaces.
    #[must_use]
    pub fn command_string(&self) -> String {
        self.command.join(" ")
    }

    /// Create a synthetic task for artifact aggregation.
    ///
    /// Used when converting `MatrixTask` (with artifacts/params but no matrix dimensions)
    /// into an IR `Task` for the emitter.
    #[must_use]
    pub fn synthetic_aggregation(
        id: impl Into<String>,
        artifact_downloads: Vec<ArtifactDownload>,
        params: BTreeMap<String, String>,
    ) -> Self {
        Self {
            id: id.into(),
            runtime: None,
            command: vec![],
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
            artifact_downloads,
            params,
            // Phase task fields (not applicable for synthetic tasks)
            phase: None,
            label: None,
            priority: None,
            contributor: None,
            condition: None,
            provider_hints: None,
        }
    }

    /// Create a synthetic task for matrix expansion.
    ///
    /// Used when converting `MatrixTask` (with matrix dimensions)
    /// into an IR `Task` for the emitter.
    #[must_use]
    pub fn synthetic_matrix(id: impl Into<String>, matrix: MatrixConfig) -> Self {
        Self {
            id: id.into(),
            runtime: None,
            command: vec![],
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
            matrix: Some(matrix),
            artifact_downloads: vec![],
            params: BTreeMap::new(),
            // Phase task fields (not applicable for synthetic tasks)
            phase: None,
            label: None,
            priority: None,
            contributor: None,
            condition: None,
            provider_hints: None,
        }
    }
}

/// Matrix configuration for parallel job expansion
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MatrixConfig {
    /// Matrix dimensions (e.g., `{"arch": ["linux-x64", "darwin-arm64"]}`)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dimensions: BTreeMap<String, Vec<String>>,

    /// Exclude specific combinations
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<BTreeMap<String, String>>,

    /// Include additional combinations
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<BTreeMap<String, String>>,

    /// Maximum parallel jobs (0 = unlimited)
    #[serde(default)]
    pub max_parallel: usize,

    /// Fail-fast behavior (stop all jobs on first failure)
    #[serde(default = "default_fail_fast")]
    pub fail_fast: bool,
}

const fn default_fail_fast() -> bool {
    true
}

/// Artifact download configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactDownload {
    /// Name pattern for the artifact (can include matrix variables like `build-${{ matrix.arch }}`)
    pub name: String,

    /// Directory to download artifacts into
    pub path: String,

    /// Optional filter pattern for matrix variants (e.g., `"*stable"`)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub filter: String,
}

/// Secret configuration for a task
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretConfig {
    /// Source reference (e.g., CI variable name, 1Password reference)
    pub source: String,

    /// Include secret in cache key via salted HMAC
    #[serde(default)]
    pub cache_key: bool,
}

/// Resource requirements for task execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ResourceRequirements {
    /// CPU request/limit (e.g., "2", "1000m")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,

    /// Memory request/limit (e.g., "2Gi", "512Mi")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,

    /// Agent/runner tags for scheduling
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// Output artifact declaration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutputDeclaration {
    /// Path to output file/directory
    pub path: String,

    /// Storage type
    #[serde(rename = "type")]
    pub output_type: OutputType,
}

/// Output storage type
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum OutputType {
    /// Store in Content Addressable Store (default)
    #[default]
    Cas,

    /// Upload via orchestrator (e.g., GitLab artifacts, Buildkite artifacts)
    Orchestrator,
}

/// Cache policy for task execution
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CachePolicy {
    /// Read from cache, write on miss (default)
    #[default]
    Normal,

    /// Read from cache only, never write (fork PRs)
    Readonly,

    /// Always execute, write results (cache warming)
    Writeonly,

    /// No cache interaction (deployments)
    Disabled,
}

// =============================================================================
// Stage Configuration (v1.4)
// =============================================================================

/// Build stages that providers can inject tasks into
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildStage {
    /// Environment bootstrap (e.g., install Nix)
    Bootstrap,

    /// Provider setup (e.g., 1Password, Cachix, AWS credentials)
    Setup,

    /// Post-success actions (e.g., notifications, cache push)
    Success,

    /// Post-failure actions (e.g., alerts, debugging)
    Failure,
}

/// Execution condition for phase tasks
///
/// Determines when a phase task runs based on the outcome of prior tasks.
/// Used by emitters to generate conditional execution logic (e.g., `if: failure()` in GitHub Actions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskCondition {
    /// Run only if all prior tasks succeeded (default for success phase)
    OnSuccess,

    /// Run only if any prior task failed (default for failure phase)
    OnFailure,

    /// Run regardless of prior task outcomes
    Always,
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;
