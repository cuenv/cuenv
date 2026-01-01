use crate::manifest::TaskMatcher;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    pub fn is_enabled(&self) -> bool {
        match self {
            ManualTrigger::Enabled(enabled) => *enabled,
            ManualTrigger::WithInputs(inputs) => !inputs.is_empty(),
        }
    }

    /// Get the inputs if configured
    pub fn inputs(&self) -> Option<&HashMap<String, WorkflowDispatchInput>> {
        match self {
            ManualTrigger::Enabled(_) => None,
            ManualTrigger::WithInputs(inputs) => Some(inputs),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PipelineCondition {
    pub pull_request: Option<bool>,
    #[serde(default)]
    pub branch: Option<StringOrVec>,
    #[serde(default)]
    pub tag: Option<StringOrVec>,
    pub default_branch: Option<bool>,
    /// Cron expression(s) for scheduled runs
    #[serde(default)]
    pub scheduled: Option<StringOrVec>,
    /// Manual trigger configuration (bool or with inputs)
    pub manual: Option<ManualTrigger>,
    /// Release event types (e.g., ["published"])
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

/// Matrix task configuration for pipeline
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MatrixTask {
    /// Task name to run
    pub task: String,
    /// Matrix dimensions (e.g., arch: ["linux-x64", "darwin-arm64"])
    pub matrix: HashMap<String, Vec<String>>,
    /// Artifacts to download before running
    #[serde(default)]
    pub artifacts: Option<Vec<ArtifactDownload>>,
    /// Parameters to pass to the task
    #[serde(default)]
    pub params: Option<HashMap<String, String>>,
}

/// Pipeline task reference - either a simple task name or a matrix task
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum PipelineTask {
    /// Simple task reference by name
    Simple(String),
    /// Matrix task with dimensions and optional artifacts/params
    Matrix(MatrixTask),
}

impl PipelineTask {
    /// Get the task name regardless of variant
    pub fn task_name(&self) -> &str {
        match self {
            PipelineTask::Simple(name) => name,
            PipelineTask::Matrix(matrix) => &matrix.task,
        }
    }

    /// Check if this is a matrix task
    pub fn is_matrix(&self) -> bool {
        matches!(self, PipelineTask::Matrix(_))
    }

    /// Get matrix dimensions if this is a matrix task
    pub fn matrix(&self) -> Option<&HashMap<String, Vec<String>>> {
        match self {
            PipelineTask::Simple(_) => None,
            PipelineTask::Matrix(m) => Some(&m.matrix),
        }
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
    #[serde(default, skip_serializing_if = "HashMap::is_empty", rename = "with")]
    pub inputs: HashMap<String, serde_json::Value>,
}

/// Provider-specific setup step overrides
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SetupStepProviderConfig {
    /// GitHub Action to use instead of shell command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github: Option<GitHubActionConfig>,
}

/// Setup step for CI pipelines - runs before main tasks
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SetupStep {
    /// Name of the setup step (for display)
    pub name: String,

    /// Command to execute (mutually exclusive with script)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Inline script to execute (mutually exclusive with command)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,

    /// Arguments for the command
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,

    /// Environment variables for this step
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, serde_json::Value>,

    /// Provider-specific overrides (e.g., GitHub Action instead of shell)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<SetupStepProviderConfig>,
}

/// CUE-defined contributor that injects setup steps based on task matching
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Contributor {
    /// Condition for when this contributor should be active
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<TaskMatcher>,

    /// Setup steps to inject when active
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<SetupStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Pipeline {
    pub name: String,
    /// Environment for secret resolution (e.g., "production")
    pub environment: Option<String>,
    pub when: Option<PipelineCondition>,
    /// Setup steps to run before tasks
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<SetupStep>,
    /// Tasks to run - can be simple task names or matrix task objects
    #[serde(default)]
    pub tasks: Vec<PipelineTask>,
    /// Whether to derive trigger paths from task inputs.
    /// Defaults to true for branch/PR triggers, false for scheduled-only.
    pub derive_paths: Option<bool>,
    /// Pipeline-specific provider configuration (overrides CI-level defaults)
    pub provider: Option<ProviderConfig>,
}

// =============================================================================
// Stage Contributors (v1.4)
// =============================================================================

/// Build stages that contributors can inject tasks into
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildStage {
    Bootstrap,
    Setup,
    Success,
    Failure,
}

/// Activation condition for stage contributors
/// All specified conditions must be true (AND logic)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActivationCondition {
    /// Always active (no conditions)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub always: Option<bool>,

    /// Runtime type detection (active if project uses any of these runtime types)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_type: Vec<String>,

    /// Cuenv source mode detection (for cuenv installation strategy)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cuenv_source: Vec<String>,

    /// Secrets provider detection (active if environment uses any of these providers)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets_provider: Vec<String>,

    /// Provider configuration detection (active if these config paths are set)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_config: Vec<String>,

    /// Task command detection (active if any pipeline task uses these commands)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_command: Vec<String>,

    /// Task label detection (active if any pipeline task has these labels)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_labels: Vec<String>,

    /// Environment name matching (active only in these environments)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environment: Vec<String>,
}

/// Secret reference for stage tasks
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

/// Provider-specific stage task configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct StageTaskProviderConfig {
    /// GitHub Action to use instead of shell command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github: Option<GitHubActionConfig>,
}

/// A task contributed to a build stage (CUE-defined)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CueStageTask {
    /// Unique task identifier (e.g., "install-nix")
    pub id: String,

    /// Target stage (bootstrap, setup, success, failure)
    pub stage: BuildStage,

    /// Human-readable display name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Shell command to execute
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

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

    /// Dependencies on other stage tasks
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Ordering within stage (lower = earlier)
    #[serde(default = "default_priority")]
    pub priority: i32,

    /// Provider-specific overrides (e.g., GitHub Actions)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<StageTaskProviderConfig>,
}

const fn default_priority() -> i32 {
    10
}

/// Stage contributor definition (CUE-defined)
/// Contributors inject tasks into build stages based on activation conditions
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StageContributor {
    /// Contributor identifier (e.g., "nix", "1password")
    pub id: String,

    /// Activation condition (defaults to always active)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<ActivationCondition>,

    /// Tasks to contribute when active
    pub tasks: Vec<CueStageTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CI {
    pub pipelines: Vec<Pipeline>,
    /// Global provider configuration defaults
    pub provider: Option<ProviderConfig>,
    /// CUE-defined contributors that inject setup steps (legacy, task-matching based)
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub contributors: HashMap<String, Contributor>,
    /// Stage contributors that inject tasks into build stages (v1.4+)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stage_contributors: Vec<StageContributor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum StringOrVec {
    String(String),
    Vec(Vec<String>),
}

impl StringOrVec {
    /// Convert to a vector of strings
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            StringOrVec::String(s) => vec![s.clone()],
            StringOrVec::Vec(v) => v.clone(),
        }
    }

    /// Get as a single string (first element if vec)
    pub fn as_single(&self) -> Option<&str> {
        match self {
            StringOrVec::String(s) => Some(s),
            StringOrVec::Vec(v) => v.first().map(|s| s.as_str()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_or_vec() {
        let single = StringOrVec::String("value".to_string());
        assert_eq!(single.to_vec(), vec!["value"]);
        assert_eq!(single.as_single(), Some("value"));

        let multi = StringOrVec::Vec(vec!["a".to_string(), "b".to_string()]);
        assert_eq!(multi.to_vec(), vec!["a", "b"]);
        assert_eq!(multi.as_single(), Some("a"));
    }

    #[test]
    fn test_manual_trigger_bool() {
        let json = r#"{"manual": true}"#;
        let cond: PipelineCondition = serde_json::from_str(json).unwrap();
        assert!(matches!(cond.manual, Some(ManualTrigger::Enabled(true))));

        let json = r#"{"manual": false}"#;
        let cond: PipelineCondition = serde_json::from_str(json).unwrap();
        assert!(matches!(cond.manual, Some(ManualTrigger::Enabled(false))));
    }

    #[test]
    fn test_manual_trigger_with_inputs() {
        let json =
            r#"{"manual": {"tag_name": {"description": "Tag to release", "required": true}}}"#;
        let cond: PipelineCondition = serde_json::from_str(json).unwrap();

        match &cond.manual {
            Some(ManualTrigger::WithInputs(inputs)) => {
                assert!(inputs.contains_key("tag_name"));
                let input = inputs.get("tag_name").unwrap();
                assert_eq!(input.description, "Tag to release");
                assert_eq!(input.required, Some(true));
            }
            _ => panic!("Expected WithInputs variant"),
        }
    }

    #[test]
    fn test_manual_trigger_helpers() {
        let enabled = ManualTrigger::Enabled(true);
        assert!(enabled.is_enabled());
        assert!(enabled.inputs().is_none());

        let disabled = ManualTrigger::Enabled(false);
        assert!(!disabled.is_enabled());

        let mut inputs = HashMap::new();
        inputs.insert(
            "tag".to_string(),
            WorkflowDispatchInput {
                description: "Tag name".to_string(),
                required: Some(true),
                default: None,
                input_type: None,
                options: None,
            },
        );
        let with_inputs = ManualTrigger::WithInputs(inputs);
        assert!(with_inputs.is_enabled());
        assert!(with_inputs.inputs().is_some());
    }

    #[test]
    fn test_scheduled_cron_expressions() {
        // Single cron expression
        let json = r#"{"scheduled": "0 0 * * 0"}"#;
        let cond: PipelineCondition = serde_json::from_str(json).unwrap();
        match &cond.scheduled {
            Some(StringOrVec::String(s)) => assert_eq!(s, "0 0 * * 0"),
            _ => panic!("Expected single string"),
        }

        // Multiple cron expressions
        let json = r#"{"scheduled": ["0 0 * * 0", "0 12 * * *"]}"#;
        let cond: PipelineCondition = serde_json::from_str(json).unwrap();
        match &cond.scheduled {
            Some(StringOrVec::Vec(v)) => {
                assert_eq!(v.len(), 2);
                assert_eq!(v[0], "0 0 * * 0");
                assert_eq!(v[1], "0 12 * * *");
            }
            _ => panic!("Expected vec"),
        }
    }

    #[test]
    fn test_release_trigger() {
        let json = r#"{"release": ["published", "created"]}"#;
        let cond: PipelineCondition = serde_json::from_str(json).unwrap();
        assert_eq!(
            cond.release,
            Some(vec!["published".to_string(), "created".to_string()])
        );
    }

    #[test]
    fn test_pipeline_derive_paths() {
        let json = r#"{"name": "ci", "tasks": ["test"], "derivePaths": true}"#;
        let pipeline: Pipeline = serde_json::from_str(json).unwrap();
        assert_eq!(pipeline.derive_paths, Some(true));

        let json = r#"{"name": "scheduled", "tasks": ["sync"], "derivePaths": false}"#;
        let pipeline: Pipeline = serde_json::from_str(json).unwrap();
        assert_eq!(pipeline.derive_paths, Some(false));

        let json = r#"{"name": "default", "tasks": ["build"]}"#;
        let pipeline: Pipeline = serde_json::from_str(json).unwrap();
        assert_eq!(pipeline.derive_paths, None);
    }

    #[test]
    fn test_pipeline_task_simple() {
        let json = r#""build""#;
        let task: PipelineTask = serde_json::from_str(json).unwrap();
        assert!(matches!(task, PipelineTask::Simple(ref s) if s == "build"));
        assert_eq!(task.task_name(), "build");
        assert!(!task.is_matrix());
        assert!(task.matrix().is_none());
    }

    #[test]
    fn test_pipeline_task_matrix() {
        let json =
            r#"{"task": "release.build", "matrix": {"arch": ["linux-x64", "darwin-arm64"]}}"#;
        let task: PipelineTask = serde_json::from_str(json).unwrap();
        assert!(task.is_matrix());
        assert_eq!(task.task_name(), "release.build");

        let matrix = task.matrix().unwrap();
        assert!(matrix.contains_key("arch"));
        assert_eq!(matrix["arch"], vec!["linux-x64", "darwin-arm64"]);
    }

    #[test]
    fn test_pipeline_task_matrix_with_artifacts() {
        let json = r#"{
            "task": "release.publish",
            "matrix": {},
            "artifacts": [{"from": "release.build", "to": "dist", "filter": "*stable"}],
            "params": {"tag": "v1.0.0"}
        }"#;
        let task: PipelineTask = serde_json::from_str(json).unwrap();

        if let PipelineTask::Matrix(m) = task {
            assert_eq!(m.task, "release.publish");
            let artifacts = m.artifacts.unwrap();
            assert_eq!(artifacts.len(), 1);
            assert_eq!(artifacts[0].from, "release.build");
            assert_eq!(artifacts[0].to, "dist");
            assert_eq!(artifacts[0].filter, "*stable");

            let params = m.params.unwrap();
            assert_eq!(params.get("tag"), Some(&"v1.0.0".to_string()));
        } else {
            panic!("Expected Matrix variant");
        }
    }

    #[test]
    fn test_pipeline_mixed_tasks() {
        let json = r#"{
            "name": "release",
            "tasks": [
                {"task": "release.build", "matrix": {"arch": ["linux-x64", "darwin-arm64"]}},
                "release.publish:github",
                "docs.deploy"
            ]
        }"#;
        let pipeline: Pipeline = serde_json::from_str(json).unwrap();
        assert_eq!(pipeline.tasks.len(), 3);
        assert!(pipeline.tasks[0].is_matrix());
        assert!(!pipeline.tasks[1].is_matrix());
        assert!(!pipeline.tasks[2].is_matrix());
    }

    #[test]
    fn test_runner_mapping() {
        let json = r#"{"arch": {"linux-x64": "ubuntu-latest", "darwin-arm64": "macos-14"}}"#;
        let mapping: RunnerMapping = serde_json::from_str(json).unwrap();
        let arch = mapping.arch.unwrap();
        assert_eq!(arch.get("linux-x64"), Some(&"ubuntu-latest".to_string()));
        assert_eq!(arch.get("darwin-arm64"), Some(&"macos-14".to_string()));
    }
}
