//! GitHub configuration types and extension traits.
//!
//! This module provides GitHub-specific configuration types and extension traits
//! for working with GitHub Actions CI configuration.

use cuenv_core::ci::{CI, RunnerMapping, StringOrVec};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// GitHub Actions provider configuration.
///
/// This struct is owned by the GitHub crate - it should not be defined in core.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GitHubConfig {
    /// Runner label(s) - single string or array of labels
    pub runner: Option<StringOrVec>,
    /// Runner mapping for matrix dimensions
    pub runners: Option<RunnerMapping>,
    /// Cachix configuration for Nix caching
    pub cachix: Option<CachixConfig>,
    /// Artifact upload configuration
    pub artifacts: Option<ArtifactsConfig>,
    /// Trusted publishing configuration (OIDC-based, no secrets needed)
    pub trusted_publishing: Option<TrustedPublishingConfig>,
    /// Paths to ignore for trigger conditions
    pub paths_ignore: Option<Vec<String>>,
    /// Workflow permissions
    pub permissions: Option<HashMap<String, String>>,
}

/// Trusted publishing configuration for OIDC-based package publishing.
///
/// Enables publishing to package registries without storing long-lived tokens.
/// Uses short-lived tokens obtained via OIDC from the CI platform.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TrustedPublishingConfig {
    /// Enable trusted publishing for crates.io
    ///
    /// When enabled, uses `rust-lang/crates-io-auth-action` to obtain
    /// a short-lived token via OIDC for publishing to crates.io.
    pub crates_io: Option<bool>,
}

/// Cachix caching configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CachixConfig {
    /// Cachix cache name
    pub name: String,
    /// Secret name for auth token (defaults to CACHIX_AUTH_TOKEN)
    pub auth_token: Option<String>,
    /// Push filter pattern
    pub push_filter: Option<String>,
}

/// Artifact upload configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactsConfig {
    /// Paths to upload as artifacts
    pub paths: Option<Vec<String>>,
    /// Behavior when no files found: "warn", "error", or "ignore"
    pub if_no_files_found: Option<String>,
}

/// Extension trait for GitHub-specific configuration operations on [`CI`].
///
/// This trait moves GitHub-specific config merging logic from `cuenv-core`
/// to the GitHub crate where it belongs.
pub trait GitHubConfigExt {
    /// Get merged GitHub config for a specific pipeline.
    ///
    /// Pipeline-specific config overrides CI-level defaults. Fields are merged
    /// with pipeline config taking precedence over global config.
    fn github_config_for_pipeline(&self, pipeline_name: &str) -> GitHubConfig;
}

impl GitHubConfigExt for CI {
    fn github_config_for_pipeline(&self, pipeline_name: &str) -> GitHubConfig {
        let global = self
            .provider
            .as_ref()
            .and_then(|p| p.get("github"))
            .and_then(|v| serde_json::from_value::<GitHubConfig>(v.clone()).ok())
            .unwrap_or_default();

        let pipeline_config = self
            .pipelines
            .iter()
            .find(|p| p.name == pipeline_name)
            .and_then(|p| p.provider.as_ref())
            .and_then(|p| p.get("github"))
            .and_then(|v| serde_json::from_value::<GitHubConfig>(v.clone()).ok());

        match pipeline_config {
            Some(pipeline) => GitHubConfig {
                runner: pipeline.runner.clone().or(global.runner),
                runners: pipeline.runners.clone().or(global.runners),
                cachix: pipeline.cachix.clone().or(global.cachix),
                artifacts: pipeline.artifacts.clone().or(global.artifacts),
                trusted_publishing: pipeline
                    .trusted_publishing
                    .clone()
                    .or(global.trusted_publishing),
                paths_ignore: pipeline.paths_ignore.clone().or(global.paths_ignore),
                permissions: pipeline.permissions.clone().or(global.permissions),
            },
            None => global,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::ci::{Pipeline, PipelineTask};
    use serde_json::json;

    #[test]
    fn test_github_config_merge() {
        let ci = CI {
            provider: Some(
                serde_json::from_value(json!({
                    "github": {
                        "runner": "ubuntu-latest",
                        "cachix": {
                            "name": "my-cache"
                        }
                    }
                }))
                .unwrap(),
            ),
            contributors: HashMap::default(),
            stage_contributors: vec![],
            pipelines: vec![
                Pipeline {
                    name: "ci".to_string(),
                    environment: None,
                    when: None,
                    tasks: vec![PipelineTask::Simple("test".to_string())],
                    derive_paths: None,
                    setup: vec![],
                    provider: Some(
                        serde_json::from_value(json!({
                            "github": {
                                "runner": "self-hosted"
                            }
                        }))
                        .unwrap(),
                    ),
                },
                Pipeline {
                    name: "release".to_string(),
                    environment: None,
                    when: None,
                    tasks: vec![PipelineTask::Simple("deploy".to_string())],
                    derive_paths: None,
                    setup: vec![],
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
}
