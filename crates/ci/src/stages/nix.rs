//! Nix Stage Contributor
//!
//! Contributes Nix installation to the CI pipeline.

use super::StageContributor;
use crate::ir::{BuildStage, IntermediateRepresentation, StageTask};
use cuenv_core::config::CuenvSource;
use cuenv_core::manifest::{Project, Runtime};

/// Nix stage contributor
///
/// When active (any task uses a Nix runtime), contributes:
/// - Bootstrap: Install Nix via Determinate Systems installer
///
/// Note: This contributor is skipped when cuenv source is `homebrew` or `release`,
/// as those installation methods don't require Nix.
#[derive(Debug, Clone, Copy, Default)]
pub struct NixContributor;

impl NixContributor {
    /// Check if the cuenv source mode requires Nix
    fn cuenv_source_requires_nix(project: &Project) -> bool {
        project
            .config
            .as_ref()
            .and_then(|c| c.ci.as_ref())
            .and_then(|ci| ci.cuenv.as_ref())
            // Default is Release mode which doesn't require Nix for cuenv itself
            .is_some_and(|c| matches!(c.source, CuenvSource::Git | CuenvSource::Nix))
    }
}

impl StageContributor for NixContributor {
    fn id(&self) -> &'static str {
        "nix"
    }

    fn is_active(&self, _ir: &IntermediateRepresentation, project: &Project) -> bool {
        // Active if project uses a Nix-based runtime (Nix or Devenv)
        let has_nix_runtime = matches!(project.runtime, Some(Runtime::Nix(_) | Runtime::Devenv(_)));

        // Also check if cuenv source mode requires Nix (git or nix mode)
        let cuenv_needs_nix = Self::cuenv_source_requires_nix(project);

        // Active if either condition is true
        has_nix_runtime || cuenv_needs_nix
    }

    fn contribute(
        &self,
        ir: &IntermediateRepresentation,
        _project: &Project,
    ) -> (Vec<(BuildStage, StageTask)>, bool) {
        // Idempotency: check if already contributed
        if ir.stages.bootstrap.iter().any(|t| t.id == "install-nix") {
            return (vec![], false);
        }

        // Build provider hints for GitHub Actions
        let mut github_action = serde_json::Map::new();
        github_action.insert(
            "uses".to_string(),
            serde_json::Value::String("DeterminateSystems/nix-installer-action@v16".to_string()),
        );
        let mut inputs = serde_json::Map::new();
        inputs.insert(
            "extra-conf".to_string(),
            serde_json::Value::String("accept-flake-config = true".to_string()),
        );
        github_action.insert("inputs".to_string(), serde_json::Value::Object(inputs));

        let mut provider_hints = serde_json::Map::new();
        provider_hints.insert(
            "github_action".to_string(),
            serde_json::Value::Object(github_action),
        );

        (
            vec![
                // Bootstrap: Install Nix
                (
                    BuildStage::Bootstrap,
                    StageTask {
                        id: "install-nix".to_string(),
                        provider: "nix".to_string(),
                        label: Some("Install Nix".to_string()),
                        command: vec![
                            concat!(
                                "curl --proto '=https' --tlsv1.2 -sSf -L ",
                                "https://install.determinate.systems/nix | ",
                                "sh -s -- install linux --no-confirm --init none"
                            )
                            .to_string(),
                        ],
                        shell: true,
                        priority: 0,
                        // Provider-specific hints (GitHub Actions uses "github_action" key)
                        provider_hints: Some(serde_json::Value::Object(provider_hints)),
                        ..Default::default()
                    },
                ),
            ],
            true,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{PipelineMetadata, PurityMode, Runtime, StageConfiguration};
    use cuenv_core::config::{CIConfig, Config, CuenvConfig};

    fn make_ir_with_runtime() -> IntermediateRepresentation {
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
            runtimes: vec![Runtime {
                id: "default".to_string(),
                flake: ".".to_string(),
                output: "devShells.x86_64-linux.default".to_string(),
                system: "x86_64-linux".to_string(),
                digest: "sha256:abc123".to_string(),
                purity: PurityMode::Strict,
            }],
            stages: StageConfiguration::default(),
            tasks: vec![],
        }
    }

    fn make_ir_without_runtime() -> IntermediateRepresentation {
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

    fn make_project_with_nix_runtime() -> Project {
        Project {
            name: "test".to_string(),
            runtime: Some(cuenv_core::manifest::Runtime::Nix(
                cuenv_core::manifest::NixRuntime::default(),
            )),
            ..Default::default()
        }
    }

    fn make_project_without_runtime() -> Project {
        Project {
            name: "test".to_string(),
            ..Default::default()
        }
    }

    fn make_project_with_cuenv_source(source: CuenvSource) -> Project {
        Project {
            name: "test".to_string(),
            config: Some(Config {
                ci: Some(CIConfig {
                    cuenv: Some(CuenvConfig {
                        source,
                        version: "self".to_string(),
                    }),
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn test_is_active_with_runtime() {
        let contributor = NixContributor;
        let ir = make_ir_with_runtime();
        let project = make_project_with_nix_runtime();

        assert!(contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_is_active_without_runtime() {
        let contributor = NixContributor;
        let ir = make_ir_without_runtime();
        let project = make_project_without_runtime();

        assert!(!contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_is_active_with_git_cuenv_source() {
        let contributor = NixContributor;
        let ir = make_ir_without_runtime();
        let project = make_project_with_cuenv_source(CuenvSource::Git);

        // Git source requires Nix for building
        assert!(contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_is_active_with_nix_cuenv_source() {
        let contributor = NixContributor;
        let ir = make_ir_without_runtime();
        let project = make_project_with_cuenv_source(CuenvSource::Nix);

        // Nix source requires Nix for installation
        assert!(contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_is_inactive_with_homebrew_cuenv_source() {
        let contributor = NixContributor;
        let ir = make_ir_without_runtime();
        let project = make_project_with_cuenv_source(CuenvSource::Homebrew);

        // Homebrew source does NOT require Nix
        assert!(!contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_is_inactive_with_release_cuenv_source() {
        let contributor = NixContributor;
        let ir = make_ir_without_runtime();
        let project = make_project_with_cuenv_source(CuenvSource::Release);

        // Release source does NOT require Nix
        assert!(!contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_contribute_returns_install_nix() {
        let contributor = NixContributor;
        let ir = make_ir_with_runtime();
        let project = make_project_with_nix_runtime();

        let (contributions, modified) = contributor.contribute(&ir, &project);

        assert!(modified);
        assert_eq!(contributions.len(), 1);

        // Should be bootstrap (install-nix)
        let (stage, task) = &contributions[0];
        assert_eq!(*stage, BuildStage::Bootstrap);
        assert_eq!(task.id, "install-nix");
        assert_eq!(task.provider, "nix");
        assert_eq!(task.priority, 0);
    }

    #[test]
    fn test_install_nix_uses_determinate_systems() {
        let contributor = NixContributor;
        let ir = make_ir_with_runtime();
        let project = make_project_with_nix_runtime();

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, install_task) = &contributions[0];

        assert!(install_task.command[0].contains("install.determinate.systems"));
    }

    #[test]
    fn test_contribute_is_idempotent() {
        let contributor = NixContributor;
        let mut ir = make_ir_with_runtime();
        let project = make_project_with_nix_runtime();

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
