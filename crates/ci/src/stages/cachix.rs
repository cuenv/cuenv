//! Cachix Stage Contributor
//!
//! Contributes Cachix setup task to the CI pipeline for Nix caching.

use super::StageContributor;
use crate::ir::{BuildStage, IntermediateRepresentation, StageTask};
use cuenv_core::manifest::Project;
use std::collections::HashMap;

/// Cachix stage contributor
///
/// When active (Cachix is configured in the project's CI config), contributes:
/// - Setup: Configure Cachix for Nix binary caching
#[derive(Debug, Clone, Copy, Default)]
pub struct CachixContributor;

impl CachixContributor {
    /// Get Cachix configuration from project
    fn get_cachix_config(project: &Project) -> Option<CachixInfo> {
        let ci = project.ci.as_ref()?;
        let provider = ci.provider.as_ref()?;
        let github = provider.github.as_ref()?;
        let cachix = github.cachix.as_ref()?;

        Some(CachixInfo {
            name: cachix.name.clone(),
            auth_token_secret: cachix
                .auth_token
                .clone()
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

        let mut env = HashMap::new();
        env.insert(
            "CACHIX_AUTH_TOKEN".to_string(),
            format!("${{{}}}", config.auth_token_secret),
        );

        (
            vec![(
                BuildStage::Setup,
                StageTask {
                    id: "setup-cachix".to_string(),
                    provider: "cachix".to_string(),
                    label: Some("Setup Cachix".to_string()),
                    command: vec![format!(
                        concat!(
                            ". /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh && ",
                            "nix-env -iA cachix -f https://cachix.org/api/v1/install && ",
                            "cachix use {}"
                        ),
                        config.name
                    )],
                    shell: true,
                    env,
                    depends_on: vec!["install-nix".to_string()],
                    // Priority 5: after Nix install (0), before cuenv build (10)
                    priority: 5,
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
    use crate::ir::{PipelineMetadata, StageConfiguration};
    use cuenv_core::ci::{CI, CachixConfig, GitHubConfig, ProviderConfig};

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
                provider: Some(ProviderConfig {
                    github: Some(GitHubConfig {
                        cachix: Some(CachixConfig {
                            name: "my-cache".to_string(),
                            auth_token: None,
                            push_filter: None,
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        }
    }

    fn make_project_with_custom_token() -> Project {
        Project {
            name: "test".to_string(),
            ci: Some(CI {
                pipelines: vec![],
                provider: Some(ProviderConfig {
                    github: Some(GitHubConfig {
                        cachix: Some(CachixConfig {
                            name: "my-cache".to_string(),
                            auth_token: Some("MY_CACHIX_TOKEN".to_string()),
                            push_filter: None,
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
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
        let ir = make_ir();
        let project = make_project_with_cachix();

        assert!(contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_is_active_without_cachix() {
        let contributor = CachixContributor;
        let ir = make_ir();
        let project = make_project_without_cachix();

        assert!(!contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_contribute_returns_setup_task() {
        let contributor = CachixContributor;
        let ir = make_ir();
        let project = make_project_with_cachix();

        let (contributions, modified) = contributor.contribute(&ir, &project);

        assert!(modified);
        assert_eq!(contributions.len(), 1);

        let (stage, task) = &contributions[0];
        assert_eq!(*stage, BuildStage::Setup);
        assert_eq!(task.id, "setup-cachix");
        assert_eq!(task.provider, "cachix");
        assert_eq!(task.priority, 5);
    }

    #[test]
    fn test_contribute_uses_cache_name() {
        let contributor = CachixContributor;
        let ir = make_ir();
        let project = make_project_with_cachix();

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert!(task.command[0].contains("cachix use my-cache"));
    }

    #[test]
    fn test_contribute_default_auth_token() {
        let contributor = CachixContributor;
        let ir = make_ir();
        let project = make_project_with_cachix();

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert_eq!(
            task.env.get("CACHIX_AUTH_TOKEN").unwrap(),
            "${CACHIX_AUTH_TOKEN}"
        );
    }

    #[test]
    fn test_contribute_custom_auth_token() {
        let contributor = CachixContributor;
        let ir = make_ir();
        let project = make_project_with_custom_token();

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert_eq!(
            task.env.get("CACHIX_AUTH_TOKEN").unwrap(),
            "${MY_CACHIX_TOKEN}"
        );
    }

    #[test]
    fn test_contribute_depends_on_install_nix() {
        let contributor = CachixContributor;
        let ir = make_ir();
        let project = make_project_with_cachix();

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert!(task.depends_on.contains(&"install-nix".to_string()));
    }

    #[test]
    fn test_contribute_empty_without_config() {
        let contributor = CachixContributor;
        let ir = make_ir();
        let project = make_project_without_cachix();

        let (contributions, modified) = contributor.contribute(&ir, &project);

        assert!(!modified);
        assert!(contributions.is_empty());
    }

    #[test]
    fn test_contribute_is_idempotent() {
        let contributor = CachixContributor;
        let mut ir = make_ir();
        let project = make_project_with_cachix();

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
}
