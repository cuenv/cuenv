use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PipelineCondition {
    pub pull_request: Option<bool>,
    #[serde(default)]
    pub branch: Option<StringOrVec>,
    #[serde(default)]
    pub tag: Option<StringOrVec>,
    pub default_branch: Option<bool>,
    pub scheduled: Option<bool>,
    pub manual: Option<bool>,
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
pub struct Pipeline {
    pub name: String,
    pub when: Option<PipelineCondition>,
    pub tasks: Vec<String>,
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
                    when: None,
                    tasks: vec!["test".to_string()],
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
                    when: None,
                    tasks: vec!["deploy".to_string()],
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
}
