use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Workflow dispatch input definition for manual triggers
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

/// GitHub Actions provider configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GitHubConfig {
    /// Runner label(s) - single string or array of labels
    pub runner: Option<StringOrVec>,
    /// Cachix configuration for Nix caching
    pub cachix: Option<CachixConfig>,
    /// Artifact upload configuration
    pub artifacts: Option<ArtifactsConfig>,
    /// Paths to ignore for trigger conditions
    pub paths_ignore: Option<Vec<String>>,
    /// Workflow permissions
    pub permissions: Option<HashMap<String, String>>,
}

/// Cachix caching configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CachixConfig {
    /// Cachix cache name
    pub name: String,
    /// Secret name for auth token (defaults to CACHIX_AUTH_TOKEN)
    pub auth_token: Option<String>,
    /// Push filter pattern
    pub push_filter: Option<String>,
}

/// Artifact upload configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactsConfig {
    /// Paths to upload as artifacts
    pub paths: Option<Vec<String>>,
    /// Behavior when no files found: "warn", "error", or "ignore"
    pub if_no_files_found: Option<String>,
}

/// Buildkite provider configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct BuildkiteConfig {
    /// Default queue for agents
    pub queue: Option<String>,
    /// Enable emoji prefixes in step labels
    pub use_emojis: Option<bool>,
    /// Buildkite plugins
    pub plugins: Option<Vec<BuildkitePlugin>>,
}

/// Buildkite plugin configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuildkitePlugin {
    /// Plugin name
    pub name: String,
    /// Plugin configuration (arbitrary JSON)
    pub config: Option<serde_json::Value>,
}

/// GitLab CI provider configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GitLabConfig {
    /// Docker image for jobs
    pub image: Option<String>,
    /// Runner tags
    pub tags: Option<Vec<String>>,
    /// Cache configuration
    pub cache: Option<GitLabCacheConfig>,
}

/// GitLab cache configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GitLabCacheConfig {
    /// Cache key
    pub key: Option<String>,
    /// Paths to cache
    pub paths: Option<Vec<String>>,
}

/// Provider-specific configuration container
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProviderConfig {
    /// GitHub Actions configuration
    pub github: Option<GitHubConfig>,
    /// Buildkite configuration
    pub buildkite: Option<BuildkiteConfig>,
    /// GitLab CI configuration
    pub gitlab: Option<GitLabConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Pipeline {
    pub name: String,
    /// Environment for secret resolution (e.g., "production")
    pub environment: Option<String>,
    pub when: Option<PipelineCondition>,
    /// Manual task list (mutually exclusive with `release`)
    #[serde(default)]
    pub tasks: Vec<String>,
    /// When true, auto-generates build matrix and publish jobs from release config.
    /// Mutually exclusive with `tasks`.
    pub release: Option<bool>,
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

impl CI {
    /// Get merged GitHub config for a specific pipeline.
    /// Pipeline-specific config overrides CI-level defaults.
    pub fn github_config_for_pipeline(&self, pipeline_name: &str) -> GitHubConfig {
        let global = self
            .provider
            .as_ref()
            .and_then(|p| p.github.as_ref())
            .cloned()
            .unwrap_or_default();

        let pipeline_config = self
            .pipelines
            .iter()
            .find(|p| p.name == pipeline_name)
            .and_then(|p| p.provider.as_ref())
            .and_then(|p| p.github.as_ref());

        match pipeline_config {
            Some(pipeline) => GitHubConfig {
                runner: pipeline.runner.clone().or(global.runner),
                cachix: pipeline.cachix.clone().or(global.cachix),
                artifacts: pipeline.artifacts.clone().or(global.artifacts),
                paths_ignore: pipeline.paths_ignore.clone().or(global.paths_ignore),
                permissions: pipeline.permissions.clone().or(global.permissions),
            },
            None => global,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    fn test_github_config_merge() {
        let ci = CI {
            provider: Some(ProviderConfig {
                github: Some(GitHubConfig {
                    runner: Some(StringOrVec::String("ubuntu-latest".to_string())),
                    cachix: Some(CachixConfig {
                        name: "my-cache".to_string(),
                        auth_token: None,
                        push_filter: None,
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            pipelines: vec![
                Pipeline {
                    name: "ci".to_string(),
                    environment: None,
                    when: None,
                    tasks: vec!["test".to_string()],
                    release: None,
                    derive_paths: None,
                    provider: Some(ProviderConfig {
                        github: Some(GitHubConfig {
                            runner: Some(StringOrVec::String("self-hosted".to_string())),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                },
                Pipeline {
                    name: "release".to_string(),
                    environment: None,
                    when: None,
                    tasks: vec!["deploy".to_string()],
                    release: None,
                    derive_paths: None,
                    provider: None,
                },
            ],
        };

        // Pipeline with override
        let ci_config = ci.github_config_for_pipeline("ci");
        assert_eq!(
            ci_config.runner,
            Some(StringOrVec::String("self-hosted".to_string()))
        );
        assert!(ci_config.cachix.is_some()); // Inherited from global

        // Pipeline without override
        let release_config = ci.github_config_for_pipeline("release");
        assert_eq!(
            release_config.runner,
            Some(StringOrVec::String("ubuntu-latest".to_string()))
        );
    }

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
}
