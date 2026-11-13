//! Buildkite CI Provider
//!
//! Detects the Buildkite CI environment and compares a pull request build
//! with the branch it targets.
//!
//! No other build has a base. Buildkite reports no previous commit for a
//! branch build, so a build of a branch, of a tag, or one started from the
//! UI, the API, a trigger step or a schedule is compared with nothing and
//! runs its whole pipeline. The parent of `HEAD` is not a base: one push can
//! hold several commits. Nor is the default branch: comparing with it says
//! how a branch differs from the default branch, not what the branch gained
//! since its last build.

use async_trait::async_trait;
use cuenv_ci::context::CIContext;
use cuenv_ci::provider::git::{Fetch, RunCheckout, Since};
use cuenv_ci::provider::{CIProvider, ChangedFiles, NoBase};
use cuenv_ci::report::{CheckHandle, PipelineReport, PipelineStatus};
use cuenv_core::Result;
use std::process::Command;
use tracing::info;

/// Buildkite CI provider.
///
/// Provides CI integration for pipelines running on Buildkite.
/// Detects context from Buildkite environment variables and compares a
/// pull request build with the branch it targets.
pub struct BuildkiteCIProvider {
    context: CIContext,
    build: Build,
    checkout: RunCheckout,
}

/// What Buildkite is building, which decides what the build is compared
/// with.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Build {
    /// A pull request, compared with the branch it targets.
    PullRequest {
        /// `BUILDKITE_PULL_REQUEST_BASE_BRANCH`.
        base_branch: String,
    },
    /// A tag.
    Tag,
    /// A branch, whatever started the build.
    Branch,
}

impl BuildkiteCIProvider {
    /// Compare the build with its base, when it is of a kind that has one.
    fn compare_with_base(&self) -> ChangedFiles {
        match &self.build {
            Build::PullRequest { base_branch } => self.compare_with_branch(base_branch),
            Build::Tag => ChangedFiles::NoBase(NoBase::new("Buildkite tag build")),
            Build::Branch => ChangedFiles::NoBase(NoBase::new("Buildkite branch build")),
        }
    }

    /// Compare a pull request build with the branch it targets: what the
    /// build has that the branch does not.
    fn compare_with_branch(&self, base: &str) -> ChangedFiles {
        let is_shallow = self.checkout.is_shallow();
        info!("Pull request build, comparing against {base} (shallow clone: {is_shallow})");

        if is_shallow {
            self.checkout.fetch(Fetch::Branch(base));
        }

        match self
            .checkout
            .changed(Since::MergeBaseWith(&format!("origin/{base}")))
        {
            Some(files) => {
                info!("Found {} changed files via PR comparison", files.len());
                ChangedFiles::Compared(files)
            }
            None => ChangedFiles::NoBase(NoBase::new(format!(
                "Buildkite pull request build whose base branch '{base}' cannot be compared"
            ))),
        }
    }
}

impl Build {
    /// Read the build from Buildkite's environment variables. Buildkite
    /// sets the pull request variables to `false` or leaves them empty for a
    /// build that is not of a pull request.
    fn from_variables(variable: impl Fn(&str) -> Option<String>) -> Self {
        let set = |name: &str| variable(name).filter(|value| !value.is_empty() && value != "false");
        if let Some(base_branch) = set("BUILDKITE_PULL_REQUEST_BASE_BRANCH") {
            return Self::PullRequest { base_branch };
        }
        if set("BUILDKITE_TAG").is_some() {
            Self::Tag
        } else {
            Self::Branch
        }
    }

    /// The branch a pull request build targets.
    fn base_branch(&self) -> Option<&str> {
        match self {
            Self::PullRequest { base_branch } => Some(base_branch),
            Self::Tag | Self::Branch => None,
        }
    }
}

#[async_trait]
impl CIProvider for BuildkiteCIProvider {
    fn detect() -> Option<Self> {
        // Buildkite sets BUILDKITE=true
        if std::env::var("BUILDKITE").ok()? != "true" {
            return None;
        }

        let event = std::env::var("BUILDKITE_SOURCE").unwrap_or_else(|_| "unknown".to_string());
        let ref_name = std::env::var("BUILDKITE_BRANCH").unwrap_or_default();
        let sha = std::env::var("BUILDKITE_COMMIT").unwrap_or_else(|_| "HEAD".to_string());
        let build = Build::from_variables(|name| std::env::var(name).ok());
        let base_ref = build.base_branch().map(str::to_string);

        info!(
            "Detected Buildkite CI: branch={}, sha={}, base_ref={:?}",
            ref_name, sha, base_ref
        );

        Some(Self {
            context: CIContext {
                provider: "buildkite".to_string(),
                event,
                ref_name,
                base_ref,
                sha,
            },
            build,
            checkout: RunCheckout::default(),
        })
    }

    fn context(&self) -> &CIContext {
        &self.context
    }

    async fn changed_files(&self) -> Result<ChangedFiles> {
        Ok(self.compare_with_base())
    }

    async fn create_check(&self, name: &str) -> Result<CheckHandle> {
        // Buildkite uses annotations for status updates
        // Create an annotation with the check name
        let annotation_context = format!("cuenv-{}", name.replace(' ', "-").to_lowercase());

        info!(
            "Creating Buildkite check annotation: {}",
            annotation_context
        );

        Ok(CheckHandle {
            id: annotation_context,
        })
    }

    async fn update_check(&self, handle: &CheckHandle, summary: &str) -> Result<()> {
        // Update the annotation with progress
        let _ = Command::new("buildkite-agent")
            .args([
                "annotate",
                summary,
                "--style",
                "info",
                "--context",
                &handle.id,
            ])
            .output();

        Ok(())
    }

    async fn complete_check(&self, handle: &CheckHandle, report: &PipelineReport) -> Result<()> {
        let style = match report.status {
            PipelineStatus::Success => "success",
            PipelineStatus::Failed => "error",
            PipelineStatus::Partial | PipelineStatus::Pending => "warning",
        };

        let summary = format!(
            "## {} Pipeline: {:?}\n\nDuration: {}ms\n\nTasks: {}",
            report.project,
            report.status,
            report.duration_ms.unwrap_or(0),
            report.tasks.len()
        );

        let _ = Command::new("buildkite-agent")
            .args([
                "annotate",
                &summary,
                "--style",
                style,
                "--context",
                &handle.id,
            ])
            .output();

        info!("Completed Buildkite check: {} -> {}", handle.id, style);

        Ok(())
    }

    async fn upload_report(&self, report: &PipelineReport) -> Result<Option<String>> {
        // Write report to a temp file and upload as artifact
        let report_json = serde_json::to_string_pretty(report).unwrap_or_default();
        let report_path = format!(".cuenv/reports/{}-report.json", report.pipeline);

        if let Some(parent) = std::path::Path::new(&report_path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        if std::fs::write(&report_path, &report_json).is_ok() {
            // Upload as artifact
            let _ = Command::new("buildkite-agent")
                .args(["artifact", "upload", &report_path])
                .output();

            info!("Uploaded report artifact: {}", report_path);
            return Ok(Some(report_path));
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_not_buildkite() {
        // Without BUILDKITE env var, should return None
        temp_env::with_var_unset("BUILDKITE", || {
            assert!(BuildkiteCIProvider::detect().is_none());
        });
    }

    #[test]
    fn test_detect_buildkite() {
        temp_env::with_vars(
            [
                ("BUILDKITE", Some("true")),
                ("BUILDKITE_BRANCH", Some("main")),
                ("BUILDKITE_COMMIT", Some("abc123")),
                ("BUILDKITE_SOURCE", Some("webhook")),
            ],
            || {
                let provider = BuildkiteCIProvider::detect();
                assert!(provider.is_some());

                let provider = provider.unwrap();
                assert_eq!(provider.context.provider, "buildkite");
                assert_eq!(provider.context.ref_name, "main");
                assert_eq!(provider.context.sha, "abc123");
                assert_eq!(provider.context.event, "webhook");
            },
        );
    }

    #[test]
    fn test_detect_buildkite_pr() {
        temp_env::with_vars(
            [
                ("BUILDKITE", Some("true")),
                ("BUILDKITE_BRANCH", Some("feature/test")),
                ("BUILDKITE_COMMIT", Some("def456")),
                ("BUILDKITE_SOURCE", Some("webhook")),
                ("BUILDKITE_PULL_REQUEST_BASE_BRANCH", Some("main")),
            ],
            || {
                let provider = BuildkiteCIProvider::detect().unwrap();
                assert_eq!(provider.context.base_ref, Some("main".to_string()));
                assert_eq!(
                    provider.build,
                    Build::PullRequest {
                        base_branch: "main".to_string()
                    }
                );
            },
        );
    }

    /// The build Buildkite's variables describe.
    fn build(variables: &[(&str, &str)]) -> Build {
        Build::from_variables(|name| {
            variables
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        })
    }

    #[test]
    fn only_a_pull_request_build_has_a_base() {
        assert_eq!(
            build(&[
                ("BUILDKITE_PULL_REQUEST", "7"),
                ("BUILDKITE_PULL_REQUEST_BASE_BRANCH", "main"),
                ("BUILDKITE_PIPELINE_DEFAULT_BRANCH", "main"),
            ]),
            Build::PullRequest {
                base_branch: "main".to_string()
            }
        );

        // What Buildkite sets for a build that is not of a pull request.
        for base_branch in ["", "false"] {
            assert_eq!(
                build(&[
                    ("BUILDKITE_PULL_REQUEST", "false"),
                    ("BUILDKITE_PULL_REQUEST_BASE_BRANCH", base_branch),
                    ("BUILDKITE_PIPELINE_DEFAULT_BRANCH", "main"),
                ]),
                Build::Branch
            );
        }
        assert_eq!(build(&[]), Build::Branch);
    }

    #[test]
    fn a_tag_build_is_told_from_a_branch_build() {
        assert_eq!(build(&[("BUILDKITE_TAG", "1.2.3")]), Build::Tag);
        assert_eq!(build(&[("BUILDKITE_TAG", "")]), Build::Branch);
    }

    #[test]
    fn the_default_branch_is_not_a_base() {
        // A branch build names no base, whatever started it: the default
        // branch says how the branch differs from it, not what the branch
        // gained since its last build.
        for source in ["webhook", "ui", "api", "trigger_job", "schedule"] {
            temp_env::with_vars(
                [
                    ("BUILDKITE", Some("true")),
                    ("BUILDKITE_BRANCH", Some("staging")),
                    ("BUILDKITE_COMMIT", Some("abc123")),
                    ("BUILDKITE_SOURCE", Some(source)),
                    ("BUILDKITE_PIPELINE_DEFAULT_BRANCH", Some("main")),
                    ("BUILDKITE_PULL_REQUEST", Some("false")),
                    ("BUILDKITE_PULL_REQUEST_BASE_BRANCH", Some("")),
                    ("BUILDKITE_TAG", Some("")),
                ],
                || {
                    let provider = BuildkiteCIProvider::detect().unwrap();
                    assert_eq!(provider.context.event, source);
                    assert_eq!(provider.context.base_ref, None);
                    assert_eq!(
                        provider.compare_with_base(),
                        ChangedFiles::NoBase(NoBase::new("Buildkite branch build"))
                    );
                },
            );
        }
    }

    #[test]
    fn a_tag_build_has_no_base() {
        temp_env::with_vars(
            [
                ("BUILDKITE", Some("true")),
                ("BUILDKITE_BRANCH", Some("1.2.3")),
                ("BUILDKITE_TAG", Some("1.2.3")),
                ("BUILDKITE_PIPELINE_DEFAULT_BRANCH", Some("main")),
                ("BUILDKITE_PULL_REQUEST", Some("false")),
                ("BUILDKITE_PULL_REQUEST_BASE_BRANCH", Some("")),
            ],
            || {
                let provider = BuildkiteCIProvider::detect().unwrap();
                assert_eq!(
                    provider.compare_with_base(),
                    ChangedFiles::NoBase(NoBase::new("Buildkite tag build"))
                );
            },
        );
    }
}
