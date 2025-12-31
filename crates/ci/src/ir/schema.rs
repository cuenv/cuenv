//! IR v1.4 Schema Types
//!
//! JSON schema for the intermediate representation used by the CI pipeline compiler.
//!
//! ## Version History
//! - v1.4: Added `stages` field for provider-injected setup tasks
//! - v1.3: Initial stable version

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// IR version identifier
pub const IR_VERSION: &str = "1.4";

/// Root IR document
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntermediateRepresentation {
    /// IR version (always "1.4")
    pub version: String,

    /// Pipeline metadata
    pub pipeline: PipelineMetadata,

    /// Runtime environment definitions
    #[serde(default)]
    pub runtimes: Vec<Runtime>,

    /// Stage configuration for provider-injected tasks (bootstrap, setup, etc.)
    #[serde(default, skip_serializing_if = "StageConfiguration::is_empty")]
    pub stages: StageConfiguration,

    /// Task definitions
    pub tasks: Vec<Task>,
}

impl IntermediateRepresentation {
    /// Create a new IR document
    pub fn new(pipeline_name: impl Into<String>) -> Self {
        Self {
            version: IR_VERSION.to_string(),
            pipeline: PipelineMetadata {
                name: pipeline_name.into(),
                environment: None,
                requires_onepassword: false,
                project_name: None,
                trigger: None,
                pipeline_tasks: Vec::new(),
            },
            runtimes: Vec::new(),
            stages: StageConfiguration::default(),
            tasks: Vec::new(),
        }
    }
}

/// Pipeline metadata and trigger configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PipelineMetadata {
    /// Pipeline name
    pub name: String,

    /// Environment for secret resolution (e.g., "production")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,

    /// Whether this pipeline requires 1Password for secret resolution
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub requires_onepassword: bool,

    /// Project name (for monorepo prefixing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,

    /// Trigger conditions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<TriggerCondition>,

    /// Task IDs that this pipeline runs (for contributor filtering)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pipeline_tasks: Vec<String>,
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

    /// Path patterns to ignore (from provider config)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths_ignore: Vec<String>,
}

/// Manual trigger (`workflow_dispatch`) configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ManualTriggerConfig {
    /// Whether manual trigger is enabled
    pub enabled: bool,

    /// Input definitions for `workflow_dispatch`
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub inputs: HashMap<String, WorkflowDispatchInputDef>,
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
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,

    /// Secret configurations
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub secrets: HashMap<String, SecretConfig>,

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
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub params: HashMap<String, String>,
}

impl Task {
    /// Create a synthetic task for artifact aggregation.
    ///
    /// Used when converting `MatrixTask` (with artifacts/params but no matrix dimensions)
    /// into an IR `Task` for the emitter.
    #[must_use]
    pub fn synthetic_aggregation(
        id: impl Into<String>,
        artifact_downloads: Vec<ArtifactDownload>,
        params: HashMap<String, String>,
    ) -> Self {
        Self {
            id: id.into(),
            runtime: None,
            command: vec![],
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
            matrix: None,
            artifact_downloads,
            params,
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
            matrix: Some(matrix),
            artifact_downloads: vec![],
            params: HashMap::new(),
        }
    }
}

/// Matrix configuration for parallel job expansion
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MatrixConfig {
    /// Matrix dimensions (e.g., `{"arch": ["linux-x64", "darwin-arm64"]}`)
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub dimensions: HashMap<String, Vec<String>>,

    /// Exclude specific combinations
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<HashMap<String, String>>,

    /// Include additional combinations
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<HashMap<String, String>>,

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

/// A task contributed by a stage provider
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub struct StageTask {
    /// Unique task identifier within the stage
    pub id: String,

    /// Provider that contributed this task (e.g., "nix", "1password", "cachix")
    pub provider: String,

    /// Human-readable label for display
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Command to execute (used by all platforms unless `action` overrides for GitHub)
    pub command: Vec<String>,

    /// Shell execution mode (false = direct execve, true = wrap in /bin/sh -c)
    #[serde(default)]
    pub shell: bool,

    /// Environment variables
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,

    /// Secret configurations
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub secrets: HashMap<String, SecretConfig>,

    /// Dependencies on other stage tasks (by ID)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Priority within the stage (lower = earlier, default 0)
    #[serde(default, skip_serializing_if = "is_zero")]
    pub priority: i32,

    /// Provider-specific hints (e.g., GitHub Action specs, Buildkite plugins)
    ///
    /// This is an opaque JSON value that provider-specific emitters can interpret.
    /// For example, GitHub emitters may look for an `action` key containing
    /// `{ "uses": "actions/checkout@v4", "inputs": {...} }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_hints: Option<serde_json::Value>,
}

/// Helper to skip serializing zero priority
/// Serde's `skip_serializing_if` requires a reference parameter
#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_zero(v: &i32) -> bool {
    *v == 0
}

/// Stage configuration containing all provider-injected tasks
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StageConfiguration {
    /// Bootstrap tasks (environment setup, runs first)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bootstrap: Vec<StageTask>,

    /// Setup tasks (provider configuration, runs after bootstrap)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<StageTask>,

    /// Success tasks (post-success actions)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub success: Vec<StageTask>,

    /// Failure tasks (post-failure actions)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failure: Vec<StageTask>,
}

impl StageConfiguration {
    /// Check if all stages are empty
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bootstrap.is_empty()
            && self.setup.is_empty()
            && self.success.is_empty()
            && self.failure.is_empty()
    }

    /// Add a task to the appropriate stage
    pub fn add(&mut self, stage: BuildStage, task: StageTask) {
        match stage {
            BuildStage::Bootstrap => self.bootstrap.push(task),
            BuildStage::Setup => self.setup.push(task),
            BuildStage::Success => self.success.push(task),
            BuildStage::Failure => self.failure.push(task),
        }
    }

    /// Sort all stages by priority (lower priority = earlier)
    #[deprecated(
        note = "Use sort_by_dependencies instead, which respects depends_on relationships"
    )]
    pub fn sort_by_priority(&mut self) {
        self.bootstrap.sort_by_key(|t| t.priority);
        self.setup.sort_by_key(|t| t.priority);
        self.success.sort_by_key(|t| t.priority);
        self.failure.sort_by_key(|t| t.priority);
    }

    /// Sort all stages respecting `depends_on` relationships, with priority as tiebreaker.
    ///
    /// Uses Kahn's algorithm for topological sorting. When multiple tasks have no
    /// pending dependencies, the one with the lowest priority value is selected first.
    ///
    /// Tasks that depend on non-existent tasks are placed at the end (dependencies
    /// are considered satisfied for external/unknown dependencies).
    pub fn sort_by_dependencies(&mut self) {
        self.bootstrap = Self::topological_sort_with_priority(std::mem::take(&mut self.bootstrap));
        self.setup = Self::topological_sort_with_priority(std::mem::take(&mut self.setup));
        self.success = Self::topological_sort_with_priority(std::mem::take(&mut self.success));
        self.failure = Self::topological_sort_with_priority(std::mem::take(&mut self.failure));
    }

    /// Topological sort with priority-aware selection (Kahn's algorithm).
    ///
    /// When multiple tasks are ready (no pending dependencies), selects the one
    /// with the lowest priority value first.
    fn topological_sort_with_priority(tasks: Vec<StageTask>) -> Vec<StageTask> {
        use std::collections::{BinaryHeap, HashMap, HashSet};

        if tasks.is_empty() {
            return tasks;
        }

        // Build a set of known task IDs for filtering external dependencies
        let known_ids: HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();

        // Build adjacency list and in-degree count
        // in_degree[task_id] = number of dependencies that must complete first
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        // dependents[task_id] = list of tasks that depend on this task
        let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

        for task in &tasks {
            in_degree.entry(task.id.as_str()).or_insert(0);
            for dep in &task.depends_on {
                // Only count dependencies on known tasks (ignore external dependencies)
                if known_ids.contains(dep.as_str()) {
                    *in_degree.entry(task.id.as_str()).or_insert(0) += 1;
                    dependents
                        .entry(dep.as_str())
                        .or_default()
                        .push(task.id.as_str());
                }
            }
        }

        // Create index lookup
        let task_indices: HashMap<&str, usize> = tasks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id.as_str(), i))
            .collect();

        // Initialize ready queue with tasks that have no dependencies
        // Using (neg_priority, index) tuples - BinaryHeap is max-heap so negate priority
        let mut ready: BinaryHeap<(i32, usize)> = BinaryHeap::new();
        for (id, &degree) in &in_degree {
            if degree == 0 {
                let idx = task_indices[id];
                // Negate priority so lowest priority comes first (max-heap behavior)
                // Use negative index as secondary key for stable ordering
                ready.push((-tasks[idx].priority, usize::MAX - idx));
            }
        }

        // Kahn's algorithm
        let mut result_indices = Vec::with_capacity(tasks.len());
        let mut in_degree_mut = in_degree.clone();

        while let Some((_, neg_idx)) = ready.pop() {
            let index = usize::MAX - neg_idx;
            result_indices.push(index);
            let task_id = tasks[index].id.as_str();

            // Decrease in-degree of dependents
            if let Some(deps) = dependents.get(task_id) {
                for &dep_id in deps {
                    if let Some(degree) = in_degree_mut.get_mut(dep_id) {
                        *degree -= 1;
                        if *degree == 0 {
                            let dep_idx = task_indices[dep_id];
                            ready.push((-tasks[dep_idx].priority, usize::MAX - dep_idx));
                        }
                    }
                }
            }
        }

        // If cycle detected (not all tasks processed), fall back to priority sort
        // This shouldn't happen with valid stage configurations
        if result_indices.len() != tasks.len() {
            tracing::warn!(
                "Cycle detected in stage task dependencies, falling back to priority sort"
            );
            let mut fallback = tasks;
            fallback.sort_by_key(|t| t.priority);
            return fallback;
        }

        // Reorder tasks according to topological order
        // SAFETY: Each index appears exactly once, so each take() succeeds
        let mut tasks_vec: Vec<Option<StageTask>> = tasks.into_iter().map(Some).collect();
        result_indices
            .into_iter()
            .filter_map(|i| tasks_vec[i].take())
            .collect()
    }

    /// Get all task IDs from bootstrap and setup stages (for task dependencies)
    #[must_use]
    pub fn setup_task_ids(&self) -> Vec<String> {
        self.bootstrap
            .iter()
            .chain(self.setup.iter())
            .map(|t| t.id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ir_version() {
        let ir = IntermediateRepresentation::new("test-pipeline");
        assert_eq!(ir.version, "1.4");
        assert_eq!(ir.pipeline.name, "test-pipeline");
        assert!(ir.runtimes.is_empty());
        assert!(ir.stages.is_empty());
        assert!(ir.tasks.is_empty());
    }

    #[test]
    fn test_purity_mode_serialization() {
        let strict = PurityMode::Strict;
        let json = serde_json::to_string(&strict).unwrap();
        assert_eq!(json, r#""strict""#);

        let warning = PurityMode::Warning;
        let json = serde_json::to_string(&warning).unwrap();
        assert_eq!(json, r#""warning""#);

        let override_mode = PurityMode::Override;
        let json = serde_json::to_string(&override_mode).unwrap();
        assert_eq!(json, r#""override""#);
    }

    #[test]
    fn test_cache_policy_serialization() {
        let normal = CachePolicy::Normal;
        assert_eq!(serde_json::to_string(&normal).unwrap(), r#""normal""#);

        let readonly = CachePolicy::Readonly;
        assert_eq!(serde_json::to_string(&readonly).unwrap(), r#""readonly""#);

        let writeonly = CachePolicy::Writeonly;
        assert_eq!(serde_json::to_string(&writeonly).unwrap(), r#""writeonly""#);

        let disabled = CachePolicy::Disabled;
        assert_eq!(serde_json::to_string(&disabled).unwrap(), r#""disabled""#);
    }

    #[test]
    fn test_output_type_serialization() {
        let cas = OutputType::Cas;
        assert_eq!(serde_json::to_string(&cas).unwrap(), r#""cas""#);

        let orchestrator = OutputType::Orchestrator;
        assert_eq!(
            serde_json::to_string(&orchestrator).unwrap(),
            r#""orchestrator""#
        );
    }

    #[test]
    fn test_task_minimal() {
        let task = Task {
            id: "test-task".to_string(),
            runtime: None,
            command: vec!["echo".to_string(), "hello".to_string()],
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
            matrix: None,
            artifact_downloads: vec![],
            params: HashMap::new(),
        };

        let json = serde_json::to_value(&task).unwrap();
        assert_eq!(json["id"], "test-task");
        assert_eq!(json["command"], serde_json::json!(["echo", "hello"]));
        assert_eq!(json["shell"], false);
    }

    #[test]
    fn test_task_with_deployment() {
        let task = Task {
            id: "deploy-prod".to_string(),
            runtime: None,
            command: vec!["deploy".to_string()],
            shell: false,
            env: HashMap::new(),
            secrets: HashMap::new(),
            resources: None,
            concurrency_group: Some("production".to_string()),
            inputs: vec![],
            outputs: vec![],
            depends_on: vec!["build".to_string()],
            cache_policy: CachePolicy::Disabled,
            deployment: true,
            manual_approval: true,
            matrix: None,
            artifact_downloads: vec![],
            params: HashMap::new(),
        };

        let json = serde_json::to_value(&task).unwrap();
        assert_eq!(json["deployment"], true);
        assert_eq!(json["manual_approval"], true);
        assert_eq!(json["cache_policy"], "disabled");
        assert_eq!(json["concurrency_group"], "production");
    }

    #[test]
    fn test_task_with_matrix() {
        let task = Task {
            id: "build-matrix".to_string(),
            runtime: None,
            command: vec!["cargo".to_string(), "build".to_string()],
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
            matrix: Some(MatrixConfig {
                dimensions: [(
                    "arch".to_string(),
                    vec!["x64".to_string(), "arm64".to_string()],
                )]
                .into_iter()
                .collect(),
                ..Default::default()
            }),
            artifact_downloads: vec![],
            params: HashMap::new(),
        };

        let json = serde_json::to_value(&task).unwrap();
        assert_eq!(
            json["matrix"]["dimensions"]["arch"],
            serde_json::json!(["x64", "arm64"])
        );
    }

    #[test]
    fn test_artifact_download() {
        let artifact = ArtifactDownload {
            name: "build-${{ matrix.arch }}".to_string(),
            path: "./artifacts".to_string(),
            filter: "*stable".to_string(),
        };

        let json = serde_json::to_value(&artifact).unwrap();
        assert_eq!(json["name"], "build-${{ matrix.arch }}");
        assert_eq!(json["path"], "./artifacts");
        assert_eq!(json["filter"], "*stable");
    }

    #[test]
    fn test_secret_config() {
        let secret = SecretConfig {
            source: "CI_API_KEY".to_string(),
            cache_key: true,
        };

        let json = serde_json::to_value(&secret).unwrap();
        assert_eq!(json["source"], "CI_API_KEY");
        assert_eq!(json["cache_key"], true);
    }

    #[test]
    fn test_runtime() {
        let runtime = Runtime {
            id: "nix-rust".to_string(),
            flake: "github:NixOS/nixpkgs/nixos-unstable".to_string(),
            output: "devShells.x86_64-linux.default".to_string(),
            system: "x86_64-linux".to_string(),
            digest: "sha256:abc123".to_string(),
            purity: PurityMode::Strict,
        };

        let json = serde_json::to_value(&runtime).unwrap();
        assert_eq!(json["id"], "nix-rust");
        assert_eq!(json["purity"], "strict");
    }

    #[test]
    fn test_full_ir_serialization() {
        let mut ir = IntermediateRepresentation::new("my-pipeline");
        ir.pipeline.trigger = Some(TriggerCondition {
            branches: vec!["main".to_string()],
            ..Default::default()
        });

        ir.runtimes.push(Runtime {
            id: "default".to_string(),
            flake: "github:NixOS/nixpkgs/nixos-unstable".to_string(),
            output: "devShells.x86_64-linux.default".to_string(),
            system: "x86_64-linux".to_string(),
            digest: "sha256:def456".to_string(),
            purity: PurityMode::Warning,
        });

        ir.tasks.push(Task {
            id: "build".to_string(),
            runtime: Some("default".to_string()),
            command: vec!["cargo".to_string(), "build".to_string()],
            shell: false,
            env: HashMap::new(),
            secrets: HashMap::new(),
            resources: Some(ResourceRequirements {
                cpu: Some("2".to_string()),
                memory: Some("4Gi".to_string()),
                tags: vec!["rust".to_string()],
            }),
            concurrency_group: None,
            inputs: vec!["src/**/*.rs".to_string(), "Cargo.toml".to_string()],
            outputs: vec![OutputDeclaration {
                path: "target/release/binary".to_string(),
                output_type: OutputType::Cas,
            }],
            depends_on: vec![],
            cache_policy: CachePolicy::Normal,
            deployment: false,
            manual_approval: false,
            matrix: None,
            artifact_downloads: vec![],
            params: HashMap::new(),
        });

        let json = serde_json::to_string_pretty(&ir).unwrap();
        assert!(json.contains(r#""version": "1.4""#));
        assert!(json.contains(r#""name": "my-pipeline""#));
        assert!(json.contains(r#""id": "build""#));
    }

    // =============================================================================
    // Stage Configuration Tests (v1.4)
    // =============================================================================

    #[test]
    fn test_build_stage_serialization() {
        assert_eq!(
            serde_json::to_string(&BuildStage::Bootstrap).unwrap(),
            r#""bootstrap""#
        );
        assert_eq!(
            serde_json::to_string(&BuildStage::Setup).unwrap(),
            r#""setup""#
        );
        assert_eq!(
            serde_json::to_string(&BuildStage::Success).unwrap(),
            r#""success""#
        );
        assert_eq!(
            serde_json::to_string(&BuildStage::Failure).unwrap(),
            r#""failure""#
        );
    }

    #[test]
    fn test_stage_task_serialization() {
        let task = StageTask {
            id: "install-nix".to_string(),
            provider: "nix".to_string(),
            label: Some("Install Nix".to_string()),
            command: vec!["curl -sSf https://install.determinate.systems/nix | sh".to_string()],
            shell: true,
            env: [(
                "NIX_INSTALLER_DIAGNOSTIC_ENDPOINT".to_string(),
                String::new(),
            )]
            .into_iter()
            .collect(),
            secrets: HashMap::new(),
            depends_on: vec![],
            priority: 0,
            provider_hints: None,
        };

        let json = serde_json::to_value(&task).unwrap();
        assert_eq!(json["id"], "install-nix");
        assert_eq!(json["provider"], "nix");
        assert_eq!(json["label"], "Install Nix");
        assert_eq!(json["shell"], true);
    }

    #[test]
    fn test_stage_task_default() {
        let task = StageTask::default();
        assert!(task.id.is_empty());
        assert!(task.provider.is_empty());
        assert!(task.label.is_none());
        assert!(task.command.is_empty());
        assert!(!task.shell);
        assert!(task.env.is_empty());
        assert!(task.depends_on.is_empty());
        assert_eq!(task.priority, 0);
        assert!(task.provider_hints.is_none());
    }

    #[test]
    fn test_stage_configuration_empty() {
        let config = StageConfiguration::default();
        assert!(config.is_empty());
        assert!(config.bootstrap.is_empty());
        assert!(config.setup.is_empty());
        assert!(config.success.is_empty());
        assert!(config.failure.is_empty());
    }

    #[test]
    fn test_stage_configuration_add() {
        let mut config = StageConfiguration::default();

        config.add(
            BuildStage::Bootstrap,
            StageTask {
                id: "install-nix".to_string(),
                provider: "nix".to_string(),
                ..Default::default()
            },
        );

        config.add(
            BuildStage::Setup,
            StageTask {
                id: "setup-1password".to_string(),
                provider: "1password".to_string(),
                ..Default::default()
            },
        );

        assert!(!config.is_empty());
        assert_eq!(config.bootstrap.len(), 1);
        assert_eq!(config.setup.len(), 1);
        assert_eq!(config.bootstrap[0].id, "install-nix");
        assert_eq!(config.setup[0].id, "setup-1password");
    }

    #[test]
    fn test_stage_configuration_sort_by_priority() {
        let mut config = StageConfiguration::default();

        config.add(
            BuildStage::Setup,
            StageTask {
                id: "setup-1password".to_string(),
                priority: 20,
                ..Default::default()
            },
        );
        config.add(
            BuildStage::Setup,
            StageTask {
                id: "setup-cachix".to_string(),
                priority: 5,
                ..Default::default()
            },
        );
        config.add(
            BuildStage::Setup,
            StageTask {
                id: "setup-cuenv".to_string(),
                priority: 10,
                ..Default::default()
            },
        );

        config.sort_by_priority();

        assert_eq!(config.setup[0].id, "setup-cachix");
        assert_eq!(config.setup[1].id, "setup-cuenv");
        assert_eq!(config.setup[2].id, "setup-1password");
    }

    #[test]
    fn test_stage_configuration_setup_task_ids() {
        let mut config = StageConfiguration::default();

        config.add(
            BuildStage::Bootstrap,
            StageTask {
                id: "install-nix".to_string(),
                ..Default::default()
            },
        );
        config.add(
            BuildStage::Setup,
            StageTask {
                id: "setup-cuenv".to_string(),
                ..Default::default()
            },
        );
        config.add(
            BuildStage::Success,
            StageTask {
                id: "notify".to_string(),
                ..Default::default()
            },
        );

        let ids = config.setup_task_ids();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"install-nix".to_string()));
        assert!(ids.contains(&"setup-cuenv".to_string()));
        // Success stage should not be included
        assert!(!ids.contains(&"notify".to_string()));
    }

    #[test]
    fn test_ir_with_stages() {
        let mut ir = IntermediateRepresentation::new("ci-pipeline");

        ir.stages.add(
            BuildStage::Bootstrap,
            StageTask {
                id: "install-nix".to_string(),
                provider: "nix".to_string(),
                label: Some("Install Nix".to_string()),
                command: vec!["curl -sSf https://install.determinate.systems/nix | sh".to_string()],
                shell: true,
                priority: 0,
                ..Default::default()
            },
        );

        ir.stages.add(
            BuildStage::Setup,
            StageTask {
                id: "setup-1password".to_string(),
                provider: "1password".to_string(),
                label: Some("Setup 1Password".to_string()),
                command: vec!["cuenv secrets setup onepassword".to_string()],
                depends_on: vec!["install-nix".to_string()],
                priority: 20,
                ..Default::default()
            },
        );

        let json = serde_json::to_string_pretty(&ir).unwrap();
        assert!(json.contains(r#""version": "1.4""#));
        assert!(json.contains("install-nix"));
        assert!(json.contains("setup-1password"));
        assert!(json.contains("1password"));
    }

    #[test]
    fn test_sort_by_dependencies_respects_depends_on() {
        let mut config = StageConfiguration::default();

        // 1Password has lower priority (20) but depends on cuenv (55)
        // Without dependency-aware sorting, 1Password would come first
        config.add(
            BuildStage::Setup,
            StageTask {
                id: "setup-1password".to_string(),
                priority: 20,
                depends_on: vec!["setup-cuenv".to_string()],
                ..Default::default()
            },
        );
        config.add(
            BuildStage::Setup,
            StageTask {
                id: "setup-cuenv".to_string(),
                priority: 55,
                ..Default::default()
            },
        );
        config.add(
            BuildStage::Setup,
            StageTask {
                id: "setup-cachix".to_string(),
                priority: 5,
                ..Default::default()
            },
        );

        config.sort_by_dependencies();

        // cachix has no deps and lowest priority -> first
        assert_eq!(config.setup[0].id, "setup-cachix");
        // cuenv has no deps -> second (by priority)
        assert_eq!(config.setup[1].id, "setup-cuenv");
        // 1password depends on cuenv -> must come after cuenv
        assert_eq!(config.setup[2].id, "setup-1password");
    }

    #[test]
    fn test_sort_by_dependencies_uses_priority_as_tiebreaker() {
        let mut config = StageConfiguration::default();

        // All tasks have no dependencies, should sort by priority
        config.add(
            BuildStage::Setup,
            StageTask {
                id: "task-c".to_string(),
                priority: 30,
                ..Default::default()
            },
        );
        config.add(
            BuildStage::Setup,
            StageTask {
                id: "task-a".to_string(),
                priority: 10,
                ..Default::default()
            },
        );
        config.add(
            BuildStage::Setup,
            StageTask {
                id: "task-b".to_string(),
                priority: 20,
                ..Default::default()
            },
        );

        config.sort_by_dependencies();

        assert_eq!(config.setup[0].id, "task-a");
        assert_eq!(config.setup[1].id, "task-b");
        assert_eq!(config.setup[2].id, "task-c");
    }

    #[test]
    fn test_sort_by_dependencies_ignores_external_dependencies() {
        let mut config = StageConfiguration::default();

        // Task depends on something not in this stage (external dependency)
        config.add(
            BuildStage::Setup,
            StageTask {
                id: "setup-1password".to_string(),
                priority: 20,
                depends_on: vec!["install-nix".to_string()], // Not in setup stage
                ..Default::default()
            },
        );
        config.add(
            BuildStage::Setup,
            StageTask {
                id: "setup-cuenv".to_string(),
                priority: 55,
                ..Default::default()
            },
        );

        config.sort_by_dependencies();

        // External deps are ignored, so sort by priority
        assert_eq!(config.setup[0].id, "setup-1password");
        assert_eq!(config.setup[1].id, "setup-cuenv");
    }

    #[test]
    fn test_sort_by_dependencies_chain() {
        let mut config = StageConfiguration::default();

        // Chain: A -> B -> C (where -> means "depends on")
        config.add(
            BuildStage::Setup,
            StageTask {
                id: "task-c".to_string(),
                priority: 10, // Lowest priority, but depends on B
                depends_on: vec!["task-b".to_string()],
                ..Default::default()
            },
        );
        config.add(
            BuildStage::Setup,
            StageTask {
                id: "task-a".to_string(),
                priority: 30, // Highest priority, no deps
                ..Default::default()
            },
        );
        config.add(
            BuildStage::Setup,
            StageTask {
                id: "task-b".to_string(),
                priority: 20, // Middle priority, depends on A
                depends_on: vec!["task-a".to_string()],
                ..Default::default()
            },
        );

        config.sort_by_dependencies();

        // A has no deps -> first
        // B depends on A -> second
        // C depends on B -> third
        assert_eq!(config.setup[0].id, "task-a");
        assert_eq!(config.setup[1].id, "task-b");
        assert_eq!(config.setup[2].id, "task-c");
    }

    #[test]
    fn test_sort_by_dependencies_empty() {
        let mut config = StageConfiguration::default();
        config.sort_by_dependencies();
        assert!(config.setup.is_empty());
    }
}
