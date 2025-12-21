//! Nix Stage Contributor
//!
//! Contributes Nix installation to the CI pipeline.

use super::StageContributor;
use crate::ir::{BuildStage, IntermediateRepresentation, StageTask};
use cuenv_core::manifest::{Project, Runtime};

/// Nix stage contributor
///
/// When active (any task uses a Nix runtime), contributes:
/// - Bootstrap: Install Nix via Determinate Systems installer
#[derive(Debug, Clone, Copy, Default)]
pub struct NixContributor;

impl StageContributor for NixContributor {
    fn id(&self) -> &'static str {
        "nix"
    }

    fn is_active(&self, _ir: &IntermediateRepresentation, project: &Project) -> bool {
        // Active if project uses a Nix-based runtime (Nix or Devenv)
        matches!(
            project.runtime,
            Some(Runtime::Nix(_) | Runtime::Devenv(_))
        )
    }

    fn contribute(
        &self,
        _ir: &IntermediateRepresentation,
        _project: &Project,
    ) -> Vec<(BuildStage, StageTask)> {
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
                    ..Default::default()
                },
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{PipelineMetadata, PurityMode, Runtime, StageConfiguration};

    fn make_ir_with_runtime() -> IntermediateRepresentation {
        IntermediateRepresentation {
            version: "1.4".to_string(),
            pipeline: PipelineMetadata {
                name: "test".to_string(),
                environment: None,
                requires_onepassword: false,
                project_name: None,
                trigger: None,
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
    fn test_contribute_returns_install_nix() {
        let contributor = NixContributor;
        let ir = make_ir_with_runtime();
        let project = make_project_with_nix_runtime();

        let contributions = contributor.contribute(&ir, &project);

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

        let contributions = contributor.contribute(&ir, &project);
        let (_, install_task) = &contributions[0];

        assert!(install_task.command[0].contains("install.determinate.systems"));
    }
}
