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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Pipeline {
    pub name: String,
    /// Environment for secret resolution (e.g., "production")
    pub environment: Option<String>,
    pub when: Option<PipelineCondition>,
    /// Tasks to run - can be simple task names or matrix task objects
    #[serde(default)]
    pub tasks: Vec<PipelineTask>,
    /// Whether to derive trigger paths from task inputs.
    /// Defaults to true for branch/PR triggers, false for scheduled-only.
    pub derive_paths: Option<bool>,
    /// Pipeline-specific provider configuration (overrides CI-level defaults)
    pub provider: Option<ProviderConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CI {
    pub pipelines: Vec<Pipeline>,
    /// Global provider configuration defaults
    pub provider: Option<ProviderConfig>,
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
