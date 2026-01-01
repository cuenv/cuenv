//! Cuenv Stage Contributor
//!
//! Contributes cuenv installation/build task to the CI pipeline.

use super::StageContributor;
use crate::ir::{BuildStage, IntermediateRepresentation, StageTask};
use cuenv_core::config::CuenvSource;
use cuenv_core::manifest::Project;

/// Generate a shell command to add a path to the CI system's PATH.
///
/// This uses runtime detection to support multiple CI systems without
/// compile-time dependencies. The shell tries each known environment
/// variable in order and silently continues if none are set.
///
/// Known CI systems:
/// - GitHub Actions: `$GITHUB_PATH`
/// - Buildkite: `$BUILDKITE_ENV_FILE`
/// - GitLab CI: `$GITLAB_ENV` (future)
///
/// Falls back to `|| true` so the command never fails.
fn path_export_command(path: &str) -> String {
    // CI systems use different mechanisms for persisting PATH changes:
    // - GitHub Actions: echo to $GITHUB_PATH
    // - Buildkite: echo to $BUILDKITE_ENV_FILE with PATH= prefix
    // We try each in order; if none are set, we continue silently.
    format!(
        "{{ echo \"{path}\" >> \"$GITHUB_PATH\" 2>/dev/null || \
        echo \"{path}\" >> \"$BUILDKITE_ENV_FILE\" 2>/dev/null || true; }}"
    )
}

/// Cuenv stage contributor
///
/// Always active (cuenv is needed to run tasks). Contributes:
/// - Setup: Install or build cuenv based on configuration
///
/// The source mode is configured via `config.ci.cuenv.source`:
/// - `release` (default): Download pre-built binary from GitHub Releases
/// - `git`: Build from git checkout (requires Nix)
/// - `nix`: Install via Nix flake (auto-configures Cachix)
/// - `homebrew`: Install via Homebrew tap (no Nix required)
///
/// The version is configured via `config.ci.cuenv.version`:
/// - `self` (default): Use current checkout (for git/nix)
/// - `latest`: Latest release (for release mode)
/// - `0.17.0`: Specific version tag
#[derive(Debug, Clone, Copy, Default)]
pub struct CuenvContributor;

impl CuenvContributor {
    /// Get the cuenv configuration from the project
    fn get_config(project: &Project) -> (CuenvSource, String) {
        project
            .config
            .as_ref()
            .and_then(|c| c.ci.as_ref())
            .and_then(|ci| ci.cuenv.as_ref())
            .map_or_else(
                || (CuenvSource::Release, "latest".to_string()),
                |c| (c.source, c.version.clone()),
            )
    }

    /// Release mode: Download pre-built binary from GitHub Releases
    fn release_command(version: &str) -> String {
        let url = if version == "latest" || version == "self" {
            "https://github.com/cuenv/cuenv/releases/latest/download".to_string()
        } else {
            format!("https://github.com/cuenv/cuenv/releases/download/{version}")
        };
        format!(
            "curl -sSL -o /usr/local/bin/cuenv {url}/cuenv-linux-x64 && \
             chmod +x /usr/local/bin/cuenv"
        )
    }

    /// Git mode: Build from checkout using nix develop + cargo
    ///
    /// Uses `nix develop -c cargo build` instead of `nix build` for:
    /// - Faster builds with sccache caching
    /// - Consistent behavior with NixRuntime (which also uses nix develop)
    fn git_command(version: &str) -> String {
        if version == "self" {
            // Build from current checkout using cargo within nix develop shell
            format!(
                ". /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh && \
                nix develop -c cargo build --release -p cuenv && \
                {path_export}",
                path_export = path_export_command("$(pwd)/target/release")
            )
        } else {
            // Clone specific version and build with cargo
            format!(
                ". /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh && \
                git clone --depth 1 --branch {version} https://github.com/cuenv/cuenv.git /tmp/cuenv && \
                cd /tmp/cuenv && \
                nix develop -c cargo build --release -p cuenv && \
                {path_export}",
                path_export = path_export_command("/tmp/cuenv/target/release")
            )
        }
    }

    /// Nix mode: Install via flake (with Cachix support via accept-flake-config)
    fn nix_command(version: &str) -> String {
        if version == "self" {
            // Build from current checkout
            Self::git_command("self")
        } else {
            // Install from flake reference
            format!(
                ". /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh && \
                 nix profile install github:cuenv/cuenv/{version}#cuenv --accept-flake-config"
            )
        }
    }

    /// Homebrew mode: Install directly from repo (no Nix required)
    fn homebrew_command() -> String {
        "brew install cuenv/cuenv/cuenv".to_string()
    }

    /// Wrap a command with `cuenv sync -A` to ensure project synchronization.
    ///
    /// Uses the explicit binary path because `$GITHUB_PATH` changes only take
    /// effect in subsequent steps, not within the same step.
    fn with_sync(command: &str, cuenv_path: &str) -> String {
        format!("{command} && {cuenv_path} sync -A")
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

        let (source, version) = Self::get_config(project);

        // Check if sccache contributor is present (for Git/Nix modes that use cargo)
        let has_sccache = project
            .ci
            .as_ref()
            .is_some_and(|ci| ci.contributors.contains_key("sccache"));

        let (command, label, depends_on, priority) = match source {
            CuenvSource::Release => (
                Self::with_sync(&Self::release_command(&version), "/usr/local/bin/cuenv"),
                "Setup cuenv (release)",
                vec![], // No Nix dependency
                10,     // Default priority
            ),
            CuenvSource::Git => {
                let mut deps = vec!["install-nix".to_string()];
                // If sccache contributor is present, depend on it for cargo build caching
                if has_sccache {
                    deps.push("cue-setup-setup-sccache".to_string());
                }
                let cuenv_path = if version == "self" {
                    "./target/release/cuenv"
                } else {
                    "/tmp/cuenv/target/release/cuenv"
                };
                (
                    Self::with_sync(&Self::git_command(&version), cuenv_path),
                    if version == "self" {
                        "Build cuenv"
                    } else {
                        "Build cuenv (versioned)"
                    },
                    deps,
                    // Run after sccache (priority 50) when it's present
                    if has_sccache { 55 } else { 10 },
                )
            }
            CuenvSource::Nix => {
                let mut deps = vec!["install-nix".to_string()];
                // Nix self mode also uses cargo build
                let uses_cargo = version == "self";
                if uses_cargo && has_sccache {
                    deps.push("cue-setup-setup-sccache".to_string());
                }
                // Self mode uses cargo build, versioned mode uses nix profile (which adds to PATH)
                let cuenv_path = if version == "self" {
                    "./target/release/cuenv"
                } else {
                    "cuenv"
                };
                (
                    Self::with_sync(&Self::nix_command(&version), cuenv_path),
                    "Setup cuenv (nix)",
                    deps,
                    // Run after sccache when using cargo build
                    if uses_cargo && has_sccache { 55 } else { 10 },
                )
            }
            CuenvSource::Homebrew => (
                // Homebrew adds to PATH immediately
                Self::with_sync(&Self::homebrew_command(), "cuenv"),
                "Setup cuenv (homebrew)",
                vec![], // No Nix dependency!
                10,     // Default priority
            ),
        };

        // Add GITHUB_TOKEN for tool resolution during sync
        let mut env = std::collections::BTreeMap::new();
        env.insert(
            "GITHUB_TOKEN".to_string(),
            "${{ secrets.GITHUB_TOKEN }}".to_string(),
        );

        (
            vec![(
                BuildStage::Setup,
                StageTask {
                    id: "setup-cuenv".to_string(),
                    provider: "cuenv".to_string(),
                    label: Some(label.to_string()),
                    command: vec![command],
                    shell: true,
                    depends_on,
                    priority,
                    env,
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
                pipeline_tasks: vec![],
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

    fn make_project_with_source(source: CuenvSource, version: &str) -> Project {
        Project {
            name: "test".to_string(),
            config: Some(Config {
                ci: Some(CIConfig {
                    cuenv: Some(CuenvConfig {
                        source,
                        version: version.to_string(),
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
    fn test_default_is_release_mode_with_latest() {
        let contributor = CuenvContributor;
        let ir = make_ir();
        let project = make_project(); // No config

        let (contributions, modified) = contributor.contribute(&ir, &project);
        assert!(modified);
        assert_eq!(contributions.len(), 1);

        let (stage, task) = &contributions[0];
        assert_eq!(*stage, BuildStage::Setup);
        assert_eq!(task.id, "setup-cuenv");
        assert_eq!(task.label, Some("Setup cuenv (release)".to_string()));
        assert!(task.command[0].contains("releases/latest/download"));
        assert!(task.command[0].contains("&& /usr/local/bin/cuenv sync -A"));
        // Release mode has no Nix dependency
        assert!(task.depends_on.is_empty());
    }

    #[test]
    fn test_release_mode_with_specific_version() {
        let contributor = CuenvContributor;
        let ir = make_ir();
        let project = make_project_with_source(CuenvSource::Release, "0.17.0");

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert_eq!(task.label, Some("Setup cuenv (release)".to_string()));
        assert!(task.command[0].contains("releases/download/0.17.0"));
        assert!(task.command[0].contains("&& /usr/local/bin/cuenv sync -A"));
        assert!(task.depends_on.is_empty());
    }

    #[test]
    fn test_git_self_mode() {
        let contributor = CuenvContributor;
        let ir = make_ir();
        let project = make_project_with_source(CuenvSource::Git, "self");

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert_eq!(task.label, Some("Build cuenv".to_string()));
        // Uses nix develop + cargo build instead of nix build
        assert!(task.command[0].contains("nix develop -c cargo build --release"));
        assert!(task.command[0].contains("target/release"));
        assert!(task.command[0].contains("&& ./target/release/cuenv sync -A"));
        assert!(task.depends_on.contains(&"install-nix".to_string()));
    }

    #[test]
    fn test_git_versioned_mode() {
        let contributor = CuenvContributor;
        let ir = make_ir();
        let project = make_project_with_source(CuenvSource::Git, "0.17.0");

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert_eq!(task.label, Some("Build cuenv (versioned)".to_string()));
        assert!(task.command[0].contains("git clone --depth 1 --branch 0.17.0"));
        // Uses nix develop + cargo build instead of nix build
        assert!(task.command[0].contains("nix develop -c cargo build --release"));
        assert!(task.command[0].contains("target/release"));
        assert!(task.command[0].contains("&& /tmp/cuenv/target/release/cuenv sync -A"));
        assert!(task.depends_on.contains(&"install-nix".to_string()));
    }

    #[test]
    fn test_nix_self_mode() {
        let contributor = CuenvContributor;
        let ir = make_ir();
        let project = make_project_with_source(CuenvSource::Nix, "self");

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert_eq!(task.label, Some("Setup cuenv (nix)".to_string()));
        // Nix self mode uses same command as git self (cargo build)
        assert!(task.command[0].contains("nix develop -c cargo build --release"));
        assert!(task.command[0].contains("target/release"));
        assert!(task.command[0].contains("&& ./target/release/cuenv sync -A"));
        assert!(task.depends_on.contains(&"install-nix".to_string()));
    }

    #[test]
    fn test_nix_versioned_mode() {
        let contributor = CuenvContributor;
        let ir = make_ir();
        let project = make_project_with_source(CuenvSource::Nix, "0.17.0");

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert_eq!(task.label, Some("Setup cuenv (nix)".to_string()));
        assert!(task.command[0].contains("nix profile install github:cuenv/cuenv/0.17.0#cuenv"));
        // Versioned nix mode uses nix profile which updates PATH immediately
        assert!(task.command[0].contains("&& cuenv sync -A"));
        assert!(task.depends_on.contains(&"install-nix".to_string()));
    }

    #[test]
    fn test_homebrew_mode() {
        let contributor = CuenvContributor;
        let ir = make_ir();
        let project = make_project_with_source(CuenvSource::Homebrew, "ignored");

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert_eq!(task.label, Some("Setup cuenv (homebrew)".to_string()));
        assert!(task.command[0].contains("brew install cuenv/cuenv/cuenv"));
        assert!(task.command[0].contains("&& cuenv sync -A"));
    }

    #[test]
    fn test_homebrew_no_nix_dependency() {
        let contributor = CuenvContributor;
        let ir = make_ir();
        let project = make_project_with_source(CuenvSource::Homebrew, "ignored");

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        // Homebrew mode has NO Nix dependency
        assert!(
            task.depends_on.is_empty(),
            "Homebrew mode should not depend on install-nix"
        );
    }

    #[test]
    fn test_release_no_nix_dependency() {
        let contributor = CuenvContributor;
        let ir = make_ir();
        let project = make_project_with_source(CuenvSource::Release, "latest");

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        // Release mode has NO Nix dependency
        assert!(
            task.depends_on.is_empty(),
            "Release mode should not depend on install-nix"
        );
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

    #[test]
    fn test_git_mode_depends_on_sccache_when_present() {
        use cuenv_core::ci::{CI, Contributor};
        use std::collections::HashMap;

        let contributor = CuenvContributor;
        let ir = make_ir();

        // Create a project with git source AND sccache contributor
        let mut contributors = HashMap::new();
        contributors.insert(
            "sccache".to_string(),
            Contributor {
                when: None,
                setup: vec![],
            },
        );

        let project = Project {
            name: "test".to_string(),
            config: Some(Config {
                ci: Some(CIConfig {
                    cuenv: Some(CuenvConfig {
                        source: CuenvSource::Git,
                        version: "self".to_string(),
                    }),
                }),
                ..Default::default()
            }),
            ci: Some(CI {
                pipelines: vec![],
                provider: None,
                contributors,
            }),
            ..Default::default()
        };

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        // Should depend on both install-nix AND sccache
        assert!(
            task.depends_on.contains(&"install-nix".to_string()),
            "Should depend on install-nix"
        );
        assert!(
            task.depends_on
                .contains(&"cue-setup-setup-sccache".to_string()),
            "Should depend on sccache when contributor is present"
        );
        // Priority should be 55 (after sccache at 50) when sccache is present
        assert_eq!(
            task.priority, 55,
            "Priority should be 55 to run after sccache (priority 50)"
        );
    }

    #[test]
    fn test_git_mode_no_sccache_dependency_without_contributor() {
        let contributor = CuenvContributor;
        let ir = make_ir();
        let project = make_project_with_source(CuenvSource::Git, "self");

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        // Should depend on install-nix but NOT sccache (no contributor)
        assert!(task.depends_on.contains(&"install-nix".to_string()));
        assert!(
            !task
                .depends_on
                .contains(&"cue-setup-setup-sccache".to_string()),
            "Should not depend on sccache when contributor is absent"
        );
    }
}
