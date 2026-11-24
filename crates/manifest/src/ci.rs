use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

use crate::tasks::TaskNode;

// =============================================================================
// Annotation Values (for CI report annotations)
// =============================================================================

/// A pipeline annotation value - can be a literal string or a capture reference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum AnnotationValue {
    /// Reference to a task capture (resolved after execution)
    CaptureRef {
        /// Marker distinguishing capture references during deserialization
        #[serde(rename = "cuenvCaptureRef")]
        cuenv_capture_ref: bool,
        /// Task path whose captures are referenced
        #[serde(rename = "cuenvTask")]
        cuenv_task: String,
        /// Name of the referenced capture
        #[serde(rename = "cuenvCapture")]
        cuenv_capture: String,
    },
    /// Literal string value
    Literal(String),
}

/// Workflow dispatch input definition for manual triggers
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDispatchInput {
    /// Description shown in the GitHub UI
    pub description: String,
    /// Whether this input is required
    pub required: Option<bool>,
    /// Default value for the input
    pub default: Option<String>,
    /// Input type: "string", "boolean", "choice", or "environment"
    #[serde(rename = "type")]
    pub input_type: Option<String>,
    /// Options for choice-type inputs
    pub options: Option<Vec<String>>,
}

/// Manual trigger configuration - can be a simple bool or include inputs
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ManualTrigger {
    /// Simple enabled/disabled flag
    Enabled(bool),
    /// Workflow dispatch with input definitions
    WithInputs(HashMap<String, WorkflowDispatchInput>),
}

impl ManualTrigger {
    /// Check if manual trigger is enabled (either directly or via inputs)
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        match self {
            Self::Enabled(enabled) => *enabled,
            Self::WithInputs(inputs) => !inputs.is_empty(),
        }
    }

    /// Get the inputs if configured
    #[must_use]
    pub fn inputs(&self) -> Option<&HashMap<String, WorkflowDispatchInput>> {
        match self {
            Self::Enabled(_) => None,
            Self::WithInputs(inputs) => Some(inputs),
        }
    }
}

/// Detailed pull request trigger configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestTriggerConfig {
    /// Activity types to trigger on (e.g., `["opened", "edited", "synchronize", "reopened"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<String>,
    /// Target branches to filter on (`None` emits no branch filter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branches: Option<Vec<String>>,
    /// Explicit path filters (overrides `derivePaths` when set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
    /// Path patterns to ignore (`paths-ignore`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths_ignore: Vec<String>,
}

/// Pull request trigger configuration - simple bool or detailed object.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum PullRequestTrigger {
    /// Simple enabled/disabled flag (inherits pipeline branches and derived paths).
    Enabled(bool),
    /// Detailed pull_request trigger configuration.
    Detailed(PullRequestTriggerConfig),
}

impl PullRequestTrigger {
    /// Returns `true` if pull request triggers are enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        match self {
            Self::Enabled(enabled) => *enabled,
            Self::Detailed(_) => true,
        }
    }

    /// Returns the detailed config if this is the object form.
    #[must_use]
    pub const fn as_detailed(&self) -> Option<&PullRequestTriggerConfig> {
        match self {
            Self::Enabled(_) => None,
            Self::Detailed(cfg) => Some(cfg),
        }
    }
}

/// Detailed merge group trigger configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct MergeGroupTriggerConfig {
    /// Activity types to trigger on (defaults to `["checks_requested"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<String>,
}

/// Merge queue (`merge_group`) trigger configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum MergeGroupTrigger {
    /// Simple enabled/disabled flag (`true` emits `types: [checks_requested]`).
    Enabled(bool),
    /// Detailed merge_group trigger configuration.
    Detailed(MergeGroupTriggerConfig),
}

impl MergeGroupTrigger {
    /// Resolve enabled merge_group activity types, or `None` when disabled.
    #[must_use]
    pub fn resolved_types(&self) -> Option<Vec<String>> {
        match self {
            Self::Enabled(false) => None,
            Self::Enabled(true) => Some(vec!["checks_requested".to_string()]),
            Self::Detailed(cfg) => {
                if cfg.types.is_empty() {
                    Some(vec!["checks_requested".to_string()])
                } else {
                    Some(cfg.types.clone())
                }
            }
        }
    }
}

/// Workflow run (`workflow_run`) trigger configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunTrigger {
    /// Upstream workflow names that trigger this pipeline.
    pub workflows: Vec<String>,
    /// Activity types (defaults to `["completed"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<String>,
    /// Optional branch filter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<String>,
}

/// Trigger conditions controlling when a pipeline runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PipelineCondition {
    /// Run on pull requests (bool shorthand or detailed object)
    pub pull_request: Option<PullRequestTrigger>,
    /// Run on GitHub merge queue (`merge_group`) events
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_group: Option<MergeGroupTrigger>,
    /// Chain after upstream GitHub workflows (`workflow_run`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_run: Option<WorkflowRunTrigger>,
    /// Branch name(s) that trigger the pipeline
    #[serde(default)]
    pub branch: Option<StringOrVec>,
    /// Tag pattern(s) that trigger the pipeline
    #[serde(default)]
    pub tag: Option<StringOrVec>,
    /// Run on pushes to the default branch
    pub default_branch: Option<bool>,
    /// Cron expression(s) for scheduled runs
    #[serde(default)]
    pub scheduled: Option<StringOrVec>,
    /// Manual trigger configuration (bool or with inputs)
    pub manual: Option<ManualTrigger>,
    /// Release event types (e.g., `["published"]`)
    pub release: Option<Vec<String>>,
}

/// Runner mapping for matrix dimensions
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RunnerMapping {
    /// Architecture to runner mapping (e.g., "linux-x64" -> "ubuntu-latest")
    pub arch: Option<HashMap<String, String>>,
}

/// Artifact download configuration for pipeline tasks
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactDownload {
    /// Source task name (must have outputs)
    pub from: String,
    /// Base directory to download artifacts into
    pub to: String,
    /// Glob pattern to filter matrix variants (e.g., "*stable")
    #[serde(default)]
    pub filter: String,
}

/// A task reference - an embedded task with `_name` field injected by enrichment.
///
/// When CUE evaluates a task reference (e.g., `task: build`), it embeds the full
/// task definition. The Rust enrichment layer injects `_name` to identify the task.
///
/// Only accepts objects with `_name` field - string task names are not supported.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TaskRef {
    /// The task name (injected during enrichment based on CUE reference)
    #[serde(rename = "_name")]
    pub name: String,

    // Other fields are captured but not used - we only need the name
    #[serde(flatten)]
    _rest: serde_json::Value,
}

impl<'de> serde::Deserialize<'de> for TaskRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, Visitor};

        struct TaskRefVisitor;

        impl<'de> Visitor<'de> for TaskRefVisitor {
            type Value = TaskRef;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an object with _name field (task reference)")
            }

            fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                // Deserialize as a JSON object and extract _name
                let value: serde_json::Value =
                    serde::Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))?;

                let name = value
                    .get("_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| de::Error::missing_field("_name"))?
                    .to_string();

                Ok(TaskRef { name, _rest: value })
            }
        }

        deserializer.deserialize_map(TaskRefVisitor)
    }
}

impl TaskRef {
    /// Create a new TaskRef from a task name (for testing only)
    #[must_use]
    pub fn from_name(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            _rest: serde_json::Value::Null,
        }
    }

    /// Get the task name
    #[must_use]
    pub fn task_name(&self) -> &str {
        &self.name
    }
}

/// Matrix task configuration for pipeline
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MatrixTask {
    /// Type discriminator (always "matrix" for MatrixTask)
    /// Used by CUE to distinguish from #TaskNode in the #PipelineTask disjunction
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
    /// Task reference (CUE ref enriched with _name)
    pub task: TaskRef,
    /// Matrix dimensions (e.g., `arch: ["linux-x64", "darwin-arm64"]`)
    pub matrix: BTreeMap<String, Vec<String>>,
    /// Artifacts to download before running
    #[serde(default)]
    pub artifacts: Option<Vec<ArtifactDownload>>,
    /// Parameters to pass to the task
    #[serde(default)]
    pub params: Option<BTreeMap<String, String>>,
}

/// Pipeline task reference - either a direct task reference or a matrix task
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum PipelineTask {
    /// Matrix task with dimensions and optional artifacts/params
    /// Note: Matrix must come first in untagged enum because it has more specific fields
    Matrix(MatrixTask),
    /// Simple task reference (enriched CUE ref with _name)
    /// Note: Simple must come before Node because TaskRef requires _name field
    Simple(TaskRef),
    /// Task node (Task, TaskGroup, or Sequence) - for inline task definitions
    Node(TaskNode),
}

impl PipelineTask {
    /// Get the task name regardless of variant.
    ///
    /// For `Simple`, this returns the task reference name.
    /// For `Node(TaskNode::Task)` and `Node(TaskNode::Group)`, this returns the first child task name.
    /// For `Node(TaskNode::Sequence)`, this returns the first task's name.
    /// This allows task group expansion to work with inline task definitions.
    #[must_use]
    pub fn task_name(&self) -> &str {
        match self {
            Self::Matrix(matrix) => matrix.task.task_name(),
            Self::Simple(task_ref) => task_ref.task_name(),
            Self::Node(node) => Self::extract_task_name_from_node(node),
        }
    }

    /// Extract a task name from a TaskNode
    fn extract_task_name_from_node(node: &TaskNode) -> &str {
        match node {
            TaskNode::Task(task) => {
                // For inline tasks, use description as a fallback name
                task.description.as_deref().unwrap_or("unnamed-task")
            }
            TaskNode::Group(group) => {
                // For groups, use the first child's name
                group
                    .children
                    .keys()
                    .next()
                    .map_or("unnamed-group", String::as_str)
            }
            TaskNode::Sequence(sequence) => {
                // For sequences, recursively get the first task's name
                sequence
                    .first()
                    .map_or("unnamed-sequence", Self::extract_task_name_from_node)
            }
        }
    }

    /// Get all child task names for groups, or empty vec for simple tasks
    #[must_use]
    pub fn child_task_names(&self) -> Vec<&str> {
        match self {
            Self::Matrix(_) | Self::Simple(_) => vec![],
            Self::Node(node) => Self::extract_child_names_from_node(node),
        }
    }

    /// Extract child task names from a TaskNode
    fn extract_child_names_from_node(node: &TaskNode) -> Vec<&str> {
        match node {
            TaskNode::Task(_) => vec![],
            TaskNode::Group(group) => group.children.keys().map(String::as_str).collect(),
            TaskNode::Sequence(sequence) => sequence
                .iter()
                .flat_map(Self::extract_child_names_from_node)
                .collect(),
        }
    }

    /// Check if this is a matrix task (Matrix variant, regardless of dimensions)
    #[must_use]
    pub fn is_matrix(&self) -> bool {
        matches!(self, Self::Matrix(_))
    }

    /// Check if this is a task node (inline definition)
    #[must_use]
    pub fn is_node(&self) -> bool {
        matches!(self, Self::Node(_))
    }

    /// Check if this task has actual matrix dimensions that require expansion.
    ///
    /// Returns true only for Matrix tasks with non-empty matrix map.
    /// Aggregation tasks (empty matrix with artifacts) return false.
    #[must_use]
    pub fn has_matrix_dimensions(&self) -> bool {
        match self {
            Self::Simple(_) | Self::Node(_) => false,
            Self::Matrix(m) => !m.matrix.is_empty(),
        }
    }

    /// Get matrix dimensions if this is a matrix task
    #[must_use]
    pub fn matrix(&self) -> Option<&BTreeMap<String, Vec<String>>> {
        match self {
            Self::Simple(_) | Self::Node(_) => None,
            Self::Matrix(m) => Some(&m.matrix),
        }
    }

    /// Get the TaskNode if this is a Node variant
    #[must_use]
    pub fn as_node(&self) -> Option<&TaskNode> {
        match self {
            Self::Node(node) => Some(node),
            Self::Matrix(_) | Self::Simple(_) => None,
        }
    }

    /// Check if this is a simple task reference
    #[must_use]
    pub fn is_simple(&self) -> bool {
        matches!(self, Self::Simple(_))
    }
}

/// Provider-specific configuration container.
///
/// This is a dynamic map of provider name to provider-specific configuration.
/// Each provider crate (cuenv-github, cuenv-buildkite, cuenv-gitlab) defines
/// its own typed configuration and deserializes from this map.
///
/// Example CUE configuration:
/// ```cue
/// provider: {
///     github: {
///         runner: "ubuntu-latest"
///         cachix: { name: "my-cache" }
///     }
/// }
/// ```
pub type ProviderConfig = HashMap<String, serde_json::Value>;

/// GitHub Action configuration for setup steps
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitHubActionConfig {
    /// Action reference (e.g., "Mozilla-Actions/sccache-action@v0.2")
    pub uses: String,

    /// Action inputs (optional)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty", rename = "with")]
    pub inputs: BTreeMap<String, serde_json::Value>,

    /// GitHub Actions step condition (optional)
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "if")]
    pub if_condition: Option<String>,
}

/// Pipeline generation mode
///
/// Controls how the CI workflow is generated:
/// - `Thin`: Minimal workflow with cuenv orchestration (default)
/// - `Expanded`: Full workflow with all tasks as individual jobs/steps
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PipelineMode {
    /// Generate minimal workflow with cuenv ci orchestration
    /// Structure: bootstrap contributors → cuenv ci --pipeline <name> → finalizer contributors
    #[default]
    Thin,
    /// Generate full workflow with all tasks as individual jobs/steps
    /// Structure: All tasks expanded inline with proper dependencies
    Expanded,
}

/// Per-event cancel-in-progress configuration for workflow concurrency.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CancelInProgressByEvent {
    /// Cancel superseded pull request runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<bool>,
    /// Cancel superseded push runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<bool>,
    /// Cancel superseded merge group runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_group: Option<bool>,
    /// Cancel superseded `workflow_run` runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_run: Option<bool>,
    /// Cancel superseded manual (`workflow_dispatch`) runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual: Option<bool>,
    /// Cancel superseded scheduled runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled: Option<bool>,
    /// Cancel superseded release runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<bool>,
}

/// How `cancel-in-progress` is configured in a concurrency block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum CancelInProgressConfig {
    /// Static boolean (`true` or `false`).
    Bool(bool),
    /// Expression string (e.g., `${{ github.event_name == 'pull_request' }}`).
    Expression(String),
    /// Per-event map rendered into a GitHub Actions expression.
    ByEvent(CancelInProgressByEvent),
}

/// Workflow or job concurrency configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PipelineConcurrency {
    /// Concurrency group name or expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Whether to cancel in-progress runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_in_progress: Option<CancelInProgressConfig>,
}

/// A named CI pipeline: trigger conditions plus the tasks it runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Pipeline {
    /// Generation mode for this pipeline (default: thin)
    #[serde(default)]
    pub mode: PipelineMode,
    /// CI providers to emit workflows for (overrides global ci.providers for this pipeline).
    /// If specified, completely replaces the global providers list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
    /// Environment for secret resolution (e.g., "production")
    pub environment: Option<String>,
    /// Trigger conditions for this pipeline
    pub when: Option<PipelineCondition>,
    /// Tasks to run - can be simple task names or matrix task objects
    #[serde(default)]
    pub tasks: Vec<PipelineTask>,
    /// Key-value annotations surfaced in CI reports and job summaries.
    /// Values can be literal strings or capture references resolved after execution.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub annotations: HashMap<String, AnnotationValue>,
    /// When true, a failing task does not abort the rest of the pipeline:
    /// subsequent tasks whose dependencies failed are reported as `Skipped`
    /// (with the failing dependency named) instead of being silently dropped.
    /// The pipeline still exits non-zero overall if any task failed.
    /// Defaults to `false` (current behaviour — first failure aborts).
    #[serde(default)]
    pub continue_on_error: bool,
    /// Workflow-level concurrency configuration (overrides the default concurrency block).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<PipelineConcurrency>,
    /// Per-label job parallelism caps in expanded pipelines (e.g., `heavy: 2`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub limits: BTreeMap<String, u32>,
    /// Whether to derive trigger paths from task inputs.
    /// Defaults to true for branch/PR triggers, false for scheduled-only.
    pub derive_paths: Option<bool>,
    /// Pipeline-specific provider configuration (overrides CI-level defaults)
    pub provider: Option<ProviderConfig>,
}

// =============================================================================
// Contributors
// =============================================================================

/// Execution condition for contributor tasks
///
/// Determines when a task runs based on the outcome of prior tasks.
/// Used by emitters to generate conditional execution logic.
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

/// Activation condition for contributors
/// All specified conditions must be true (AND logic)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActivationCondition {
    /// `false` disables the contributor; `true` leaves other conditions in effect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub always: Option<bool>,

    /// Workspace membership detection (active if project is member of these workspace types)
    /// Values: "npm", "bun", "pnpm", "yarn", "cargo", "deno"
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_member: Vec<String>,

    /// Runtime type detection (active if project uses any of these runtime types)
    /// Values: "nix", "devenv", "container", "oci", "tools"
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_type: Vec<String>,

    /// Cuenv source mode detection (for cuenv installation strategy)
    /// Values: "git", "nix", "homebrew", "release", "native", "artifact"
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cuenv_source: Vec<String>,

    /// Secrets provider detection (active if environment uses any of these providers)
    /// Values: "onepassword", "infisical", "aws", "vault", "azure", "gcp"
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets_provider: Vec<String>,

    /// Provider configuration detection (active if these config paths are set)
    /// Path format: "github.cachix", "github.trustedPublishing.cratesIo"
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_config: Vec<String>,

    /// Task command detection (active if any task uses these commands)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_command: Vec<String>,

    /// Task label detection (active if a pipeline task has any listed label).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_labels: Vec<String>,

    /// Environment name matching (active only in these environments)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environment: Vec<String>,

    /// Service command detection (active if a service uses any listed program).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_command: Vec<String>,

    /// Require services when true, or require no services when false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_service: Option<bool>,
}

/// Secret reference for contributor tasks
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum SecretRef {
    /// Simple secret name (string)
    Simple(String),
    /// Detailed secret configuration
    Detailed(SecretRefConfig),
}

/// Detailed secret configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SecretRefConfig {
    /// CI secret name (e.g., "CACHIX_AUTH_TOKEN")
    pub source: String,
    /// Include in cache key via salted HMAC
    #[serde(default)]
    pub cache_key: bool,
}

/// Provider-specific task configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskProviderConfig {
    /// GitHub Action to use instead of shell command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github: Option<GitHubActionConfig>,
}

/// Auto-association rules for contributors
/// Defines how user tasks are automatically connected to contributor tasks
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AutoAssociate {
    /// Commands that trigger auto-association (e.g., `["bun", "bunx"]`)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,

    /// Task to inject as dependency (e.g., "cuenv:contributor:bun.workspace.setup")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inject_dependency: Option<String>,
}

/// Token requirements for a task or contributor step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TaskCI {
    /// GitHub-specific requirements.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<TaskGitHubConfig>,
}

/// GitHub deployment environment configuration for a task job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum TaskGitHubEnvironment {
    /// Simple environment name (or expression).
    Name(String),
    /// Environment with optional deployment URL.
    Detailed {
        /// Environment name (or expression).
        name: String,
        /// Optional deployment URL (or expression).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
}

/// Detailed GitHub job container configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskGitHubContainerConfig {
    /// Container image reference (supports digest pins).
    pub image: String,
    /// Additional Docker create options (e.g., `--ipc=host`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<String>,
    /// Environment variables set inside the container.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Volume mounts for the container.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<String>,
}

/// GitHub job container configuration (image shorthand or detailed struct).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum TaskGitHubContainer {
    /// Container image string shorthand.
    Image(String),
    /// Detailed container configuration.
    Detailed(TaskGitHubContainerConfig),
}

/// GitHub job `defaults.run` configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskGitHubRunDefaults {
    /// Default working directory for `run` steps in this job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// Default shell for `run` steps in this job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
}

/// GitHub job `defaults` configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskGitHubDefaults {
    /// Default settings for `run` steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<TaskGitHubRunDefaults>,
}

/// Job-level GitHub Actions settings lowered from `#TaskCI.github` into the IR.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskGitHubJobConfig {
    /// Per-task runner override (single label, expression string, or label list).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<StringOrVec>,
    /// Job-level concurrency configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<PipelineConcurrency>,
    /// Job timeout in minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_minutes: Option<u32>,
    /// GitHub deployment environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<TaskGitHubEnvironment>,
    /// Container image/config to run the job inside.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<TaskGitHubContainer>,
    /// Job `defaults.run` settings (`working-directory`, `shell`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<TaskGitHubDefaults>,
    /// Job-level `if` condition expression.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "if")]
    pub if_condition: Option<String>,
}

impl TaskGitHubJobConfig {
    /// Returns `true` when no job-level overrides are configured.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.runner.is_none()
            && self.concurrency.is_none()
            && self.timeout_minutes.is_none()
            && self.environment.is_none()
            && self.container.is_none()
            && self.defaults.is_none()
            && self.if_condition.is_none()
    }
}

/// GitHub token requirements and job-level settings on a task or contributor step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskGitHubConfig {
    /// Contributor steps only: emit in jobs executing these task IDs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<String>,
    /// Required token scopes and access levels.
    /// `Some({})` means an explicit empty `permissions: {}` block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<BTreeMap<String, String>>,
    /// Tasks only: how the task's job runs it. Unset uses the pipeline's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<TaskGitHubExecution>,
    /// Job-level settings (runner, concurrency, timeout, environment, container, defaults, if).
    #[serde(flatten)]
    pub job: TaskGitHubJobConfig,
}

/// How a GitHub job runs a task.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskGitHubExecution {
    /// Run the task's command in the job without `cuenv task`, so the job needs
    /// no cuenv setup and the task gets no secrets or environment through cuenv.
    Direct,
}

/// A task contributed to the DAG by a contributor (CUE-defined)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ContributorTask {
    /// Token requirements of this injected step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci: Option<TaskCI>,
    /// Task identifier (e.g., "bun.workspace.install")
    /// Will be prefixed with "cuenv:contributor:" when injected
    pub id: String,

    /// Human-readable display name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Human-readable description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Shell command to execute
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Command arguments
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,

    /// Multi-line script (alternative to command)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,

    /// Wrap command in shell
    #[serde(default)]
    pub shell: bool,

    /// Environment variables
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,

    /// Secret references (key=env var name)
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub secrets: HashMap<String, SecretRef>,

    /// Input files/patterns for caching
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<String>,

    /// Output files/patterns for caching
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,

    /// Whether task requires hermetic execution
    #[serde(default)]
    pub hermetic: bool,

    /// Dependencies on other tasks
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Ordering priority (lower = earlier)
    #[serde(default = "default_priority")]
    pub priority: i32,

    /// Execution condition (on_success, on_failure, always)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<TaskCondition>,

    /// Provider-specific overrides (e.g., GitHub Actions)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<TaskProviderConfig>,
}

const fn default_priority() -> i32 {
    10
}

/// Contributor definition (CUE-defined)
/// Contributors inject tasks into the DAG based on activation conditions
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Contributor {
    /// Contributor identifier (e.g., "bun.workspace", "nix", "1password")
    pub id: String,

    /// Activation condition (defaults to always active)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<ActivationCondition>,

    /// Tasks to contribute when active
    pub tasks: Vec<ContributorTask>,

    /// Auto-association rules for user tasks
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Auto-association of contributor setup tasks
    pub auto_associate: Option<AutoAssociate>,
}

/// Root CI configuration: providers, pipelines, and contributors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CI {
    /// CI providers to emit workflows for (e.g., `["github", "buildkite"]`).
    /// If not specified, no workflows are emitted (explicit configuration required).
    /// Per-pipeline providers can override this global setting.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
    /// Named pipelines keyed by pipeline id
    #[serde(default)]
    pub pipelines: BTreeMap<String, Pipeline>,
    /// Global provider configuration defaults
    pub provider: Option<ProviderConfig>,
    /// Contributors that inject tasks into build phases
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contributors: Vec<Contributor>,
}

impl CI {
    /// Get effective providers for a pipeline.
    ///
    /// Per-pipeline providers completely override global providers.
    /// Returns an empty slice if no providers are configured (emit nothing).
    #[must_use]
    pub fn providers_for_pipeline(&self, pipeline_name: &str) -> &[String] {
        self.pipelines
            .get(pipeline_name)
            .filter(|p| !p.providers.is_empty())
            .map_or(&self.providers, |p| p.providers.as_slice())
    }
}

/// A value that may be a single string or a list of strings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum StringOrVec {
    /// A single string value
    String(String),
    /// A list of string values
    Vec(Vec<String>),
}

impl StringOrVec {
    /// Convert to a vector of strings
    #[must_use]
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            Self::String(s) => vec![s.clone()],
            Self::Vec(v) => v.clone(),
        }
    }

    /// Get as a single string (first element if vec)
    #[must_use]
    pub fn as_single(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            Self::Vec(v) => v.first().map(|s| s.as_str()),
        }
    }
}

#[cfg(test)]
#[path = "ci_tests.rs"]
mod tests;
