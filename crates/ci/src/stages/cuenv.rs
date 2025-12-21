//! Cuenv Stage Contributor
//!
//! Contributes cuenv installation/build task to the CI pipeline.

use super::StageContributor;
use crate::ir::{BuildStage, IntermediateRepresentation, StageTask};
use cuenv_core::config::CuenvSource;
use cuenv_core::manifest::Project;

/// Cuenv stage contributor
///
/// Always active (cuenv is needed to run tasks). Contributes:
/// - Setup: Install or build cuenv based on configuration
///
/// The source mode is configured via `config.ci.cuenv.source`:
/// - `release` (default): Download latest cuenv from GitHub Releases
/// - `build`: Build from source via `nix build .#cuenv`
#[derive(Debug, Clone, Copy, Default)]
pub struct CuenvContributor;

impl CuenvContributor {
    /// Get the cuenv source configuration from the project
    fn get_source(project: &Project) -> CuenvSource {
        project
            .config
            .as_ref()
            .and_then(|c| c.ci.as_ref())
            .and_then(|ci| ci.cuenv.as_ref())
            .map(|c| c.source)
            .unwrap_or_default()
    }

    /// Generate the command for release mode (download from GitHub)
    fn release_command() -> String {
        concat!(
            ". /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh && ",
            "curl -sSL https://github.com/cuenv/cuenv/releases/latest/download/cuenv-x86_64-linux.tar.gz | ",
            "tar -xzf - -C /usr/local/bin && ",
            "chmod +x /usr/local/bin/cuenv"
        )
        .to_string()
    }

    /// Generate the command for build mode (nix build)
    fn build_command() -> String {
        concat!(
            ". /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh && ",
            "nix build .#cuenv --accept-flake-config && ",
            "echo \"$(pwd)/result/bin\" >> $BUILDKITE_ENV_FILE 2>/dev/null || ",
            "echo \"$(pwd)/result/bin\" >> $GITHUB_PATH 2>/dev/null || true"
        )
        .to_string()
    }
}

impl StageContributor for CuenvContributor {
    fn id(&self) -> &'static str {
        "cuenv"
    }

    fn is_active(&self, _ir: &IntermediateRepresentation, _project: &Project) -> bool {
        // Always active - cuenv is needed to run tasks
        true
    }

    fn contribute(
        &self,
        ir: &IntermediateRepresentation,
        project: &Project,
    ) -> (Vec<(BuildStage, StageTask)>, bool) {
        // Idempotency: check if already contributed
        if ir.stages.setup.iter().any(|t| t.id == "setup-cuenv") {
            return (vec![], false);
        }

        let source = Self::get_source(project);

        let (command, label) = match source {
            CuenvSource::Release => (Self::release_command(), "Setup cuenv"),
            CuenvSource::Build => (Self::build_command(), "Build cuenv"),
        };

        (
            vec![(
                BuildStage::Setup,
                StageTask {
                    id: "setup-cuenv".to_string(),
                    provider: "cuenv".to_string(),
                    label: Some(label.to_string()),
                    command: vec![command],
                    shell: true,
                    depends_on: vec!["install-nix".to_string()],
                    priority: 10,
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
    use cuenv_core::config::{CIConfig, Config, CuenvConfig};

    fn make_ir() -> IntermediateRepresentation {
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

    fn make_project() -> Project {
        Project {
            name: "test".to_string(),
            ..Default::default()
        }
    }

    fn make_project_with_build_source() -> Project {
        Project {
            name: "test".to_string(),
            config: Some(Config {
                ci: Some(CIConfig {
                    cuenv: Some(CuenvConfig {
                        source: CuenvSource::Build,
                    }),
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn make_project_with_release_source() -> Project {
        Project {
            name: "test".to_string(),
            config: Some(Config {
                ci: Some(CIConfig {
                    cuenv: Some(CuenvConfig {
                        source: CuenvSource::Release,
                    }),
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn test_is_always_active() {
        let contributor = CuenvContributor;
        let ir = make_ir();
        let project = make_project();

        assert!(contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_default_is_release_mode() {
        let contributor = CuenvContributor;
        let ir = make_ir();
        let project = make_project(); // No config

        let (contributions, modified) = contributor.contribute(&ir, &project);
        assert!(modified);
        assert_eq!(contributions.len(), 1);

        let (stage, task) = &contributions[0];
        assert_eq!(*stage, BuildStage::Setup);
        assert_eq!(task.id, "setup-cuenv");
        assert_eq!(task.label, Some("Setup cuenv".to_string()));
        assert!(task.command[0].contains("github.com/cuenv/cuenv/releases"));
    }

    #[test]
    fn test_explicit_release_mode() {
        let contributor = CuenvContributor;
        let ir = make_ir();
        let project = make_project_with_release_source();

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert_eq!(task.label, Some("Setup cuenv".to_string()));
        assert!(task.command[0].contains("github.com/cuenv/cuenv/releases"));
    }

    #[test]
    fn test_build_mode() {
        let contributor = CuenvContributor;
        let ir = make_ir();
        let project = make_project_with_build_source();

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert_eq!(task.label, Some("Build cuenv".to_string()));
        assert!(task.command[0].contains("nix build .#cuenv"));
    }

    #[test]
    fn test_depends_on_install_nix() {
        let contributor = CuenvContributor;
        let ir = make_ir();
        let project = make_project();

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert!(task.depends_on.contains(&"install-nix".to_string()));
    }

    #[test]
    fn test_priority_is_10() {
        let contributor = CuenvContributor;
        let ir = make_ir();
        let project = make_project();

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert_eq!(task.priority, 10);
    }

    #[test]
    fn test_contribute_is_idempotent() {
        let contributor = CuenvContributor;
        let mut ir = make_ir();
        let project = make_project();

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
