//! GitHub configuration extension traits.
//!
//! This module provides extension traits for working with GitHub-specific
//! configuration, keeping platform logic in the platform crate.

use cuenv_core::ci::{GitHubConfig, CI};

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
                runners: pipeline.runners.clone().or(global.runners),
                cachix: pipeline.cachix.clone().or(global.cachix),
                artifacts: pipeline.artifacts.clone().or(global.artifacts),
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
    use cuenv_core::ci::{
        CachixConfig, Pipeline, PipelineTask, ProviderConfig, StringOrVec,
    };

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
                    tasks: vec![PipelineTask::Simple("test".to_string())],
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
                    tasks: vec![PipelineTask::Simple("deploy".to_string())],
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
}
