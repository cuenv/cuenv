//! Cachix Stage Contributor
//!
//! Contributes Cachix setup task to the CI pipeline for Nix caching.
//!
//! This is a GitHub-specific contributor because it uses GitHub secrets
//! and the Cachix GitHub Action integration.

use crate::config::GitHubConfig;
use cuenv_ci::StageContributor;
use cuenv_ci::ir::{BuildStage, IntermediateRepresentation, SecretConfig, StageTask};
use cuenv_core::manifest::Project;
use std::collections::HashMap;

/// Cachix stage contributor
///
/// When active (Cachix is configured in the project's CI config), contributes:
/// - Setup: Configure Cachix for Nix binary caching
#[derive(Debug, Clone, Copy, Default)]
pub struct CachixContributor;

impl CachixContributor {
    /// Get Cachix configuration from project's GitHub provider config
    fn get_cachix_config(project: &Project) -> Option<CachixInfo> {
        let ci = project.ci.as_ref()?;
        let provider = ci.provider.as_ref()?;
        let github_value = provider.get("github")?;
        let github: GitHubConfig = serde_json::from_value(github_value.clone()).ok()?;
        let cachix = github.cachix?;

        Some(CachixInfo {
            name: cachix.name,
            auth_token_secret: cachix
                .auth_token
                .unwrap_or_else(|| "CACHIX_AUTH_TOKEN".to_string()),
        })
    }
}

/// Internal helper to hold Cachix configuration
struct CachixInfo {
    name: String,
    auth_token_secret: String,
}

impl StageContributor for CachixContributor {
    fn id(&self) -> &'static str {
        "cachix"
    }

    fn is_active(&self, _ir: &IntermediateRepresentation, project: &Project) -> bool {
        Self::get_cachix_config(project).is_some()
    }

    fn contribute(
        &self,
        ir: &IntermediateRepresentation,
        project: &Project,
    ) -> (Vec<(BuildStage, StageTask)>, bool) {
        // Idempotency: check if already contributed
        if ir.stages.setup.iter().any(|t| t.id == "setup-cachix") {
            return (vec![], false);
        }

        let Some(config) = Self::get_cachix_config(project) else {
            return (vec![], false);
        };

        // Build environment variables for the Cachix setup
        let mut env = HashMap::new();
        env.insert("CACHIX_CACHE_NAME".to_string(), config.name.clone());
        env.insert(
            "CACHIX_AUTH_TOKEN".to_string(),
            format!("${{{}}}", config.auth_token_secret),
        );

        // Build the cachix setup command
        // Note: The actual cachix binary is installed via nix, so we just configure and use
        let command = format!(
            ". /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh && \
             nix-env -iA cachix -f https://cachix.org/api/v1/install && \
             cachix use {}",
            config.name
        );

        (
            vec![(
                BuildStage::Setup,
                StageTask {
                    id: "setup-cachix".to_string(),
                    provider: "cachix".to_string(),
                    label: Some(format!("Setup Cachix ({})", config.name)),
                    command: vec![command],
                    shell: true,
                    env,
                    depends_on: vec!["install-nix".to_string()],
                    priority: 15, // After Nix install but before cuenv
                    secrets: HashMap::from([(
                        "CACHIX_AUTH_TOKEN".to_string(),
                        SecretConfig {
                            source: config.auth_token_secret,
                            cache_key: false,
                        },
                    )]),
                    ..Default::default()
                },
            )],
            true,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_ci::ir::{PipelineMetadata, StageConfiguration};
    use cuenv_core::ci::CI;
    use serde_json::json;

    fn make_ir() -> IntermediateRepresentation {
        IntermediateRepresentation {
            version: "1.4".to_string(),
            pipeline: PipelineMetadata {
                name: "test".to_string(),
                environment: None,
                requires_onepassword: false,
                project_name: None,
                trigger: None,
                pipeline_tasks: vec![],
            },
            runtimes: vec![],
            stages: StageConfiguration::default(),
            tasks: vec![],
        }
    }

    fn make_project_with_cachix() -> Project {
        Project {
            name: "test".to_string(),
            ci: Some(CI {
                pipelines: vec![],
                contributors: HashMap::default(),
                provider: Some(
                    serde_json::from_value(json!({
                        "github": {
                            "cachix": {
                                "name": "my-cache"
                            }
                        }
                    }))
                    .unwrap(),
                ),
            }),
            ..Default::default()
        }
    }

    fn make_project_with_custom_token() -> Project {
        Project {
            name: "test".to_string(),
            ci: Some(CI {
                pipelines: vec![],
                contributors: HashMap::default(),
                provider: Some(
                    serde_json::from_value(json!({
                        "github": {
                            "cachix": {
                                "name": "my-cache",
                                "authToken": "MY_CACHIX_TOKEN"
                            }
                        }
                    }))
                    .unwrap(),
                ),
            }),
            ..Default::default()
        }
    }

    fn make_project_without_cachix() -> Project {
        Project {
            name: "test".to_string(),
            ci: None,
            ..Default::default()
        }
    }

    #[test]
    fn test_is_active_with_cachix() {
        let contributor = CachixContributor;
        let project = make_project_with_cachix();
        let ir = make_ir();

        assert!(contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_is_inactive_without_cachix() {
        let contributor = CachixContributor;
        let project = make_project_without_cachix();
        let ir = make_ir();

        assert!(!contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_contribute_returns_setup_task() {
        let contributor = CachixContributor;
        let project = make_project_with_cachix();
        let ir = make_ir();

        let (contributions, modified) = contributor.contribute(&ir, &project);

        assert!(modified);
        assert_eq!(contributions.len(), 1);

        let (stage, task) = &contributions[0];
        assert_eq!(*stage, BuildStage::Setup);
        assert_eq!(task.id, "setup-cachix");
        assert_eq!(task.provider, "cachix");
        assert!(task.label.as_ref().unwrap().contains("my-cache"));
    }

    #[test]
    fn test_contribute_uses_default_token_secret() {
        let contributor = CachixContributor;
        let project = make_project_with_cachix();
        let ir = make_ir();

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        let secret = task.secrets.get("CACHIX_AUTH_TOKEN").unwrap();
        assert_eq!(secret.source, "CACHIX_AUTH_TOKEN");
    }

    #[test]
    fn test_contribute_uses_custom_token_secret() {
        let contributor = CachixContributor;
        let project = make_project_with_custom_token();
        let ir = make_ir();

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        let secret = task.secrets.get("CACHIX_AUTH_TOKEN").unwrap();
        assert_eq!(secret.source, "MY_CACHIX_TOKEN");
    }

    #[test]
    fn test_contribute_is_idempotent() {
        let contributor = CachixContributor;
        let project = make_project_with_cachix();
        let mut ir = make_ir();

        // First contribution should modify
        let (contributions, modified) = contributor.contribute(&ir, &project);
        assert!(modified);
        assert_eq!(contributions.len(), 1);

        // Add the task to IR
        for (stage, task) in contributions {
            ir.stages.add(stage, task);
        }

        // Second contribution should not modify
        let (contributions, modified) = contributor.contribute(&ir, &project);
        assert!(!modified);
        assert!(contributions.is_empty());
    }

    #[test]
    fn test_contribute_depends_on_nix() {
        let contributor = CachixContributor;
        let project = make_project_with_cachix();
        let ir = make_ir();

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert!(task.depends_on.contains(&"install-nix".to_string()));
    }
}
