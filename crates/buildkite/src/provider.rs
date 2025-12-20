//! Buildkite CI Provider
//!
//! Detects Buildkite CI environment and provides changed files detection
//! using Buildkite environment variables.

use async_trait::async_trait;
use cuenv_ci::context::CIContext;
use cuenv_ci::provider::CIProvider;
use cuenv_ci::report::{CheckHandle, PipelineReport, PipelineStatus};
use cuenv_core::Result;
use std::path::PathBuf;
use std::process::Command;
use tracing::{debug, info, warn};

/// Buildkite CI provider.
///
/// Provides CI integration for pipelines running on Buildkite.
/// Detects context from Buildkite environment variables and uses
/// git to determine changed files.
pub struct BuildkiteCIProvider {
    context: CIContext,
}

impl BuildkiteCIProvider {
    /// Get the base ref for comparison.
    ///
    /// For pull requests, uses `BUILDKITE_PULL_REQUEST_BASE_BRANCH`.
    /// For regular builds, attempts to use the default branch.
    fn get_base_ref() -> Option<String> {
        // For PRs, use the base branch
        if let Ok(base) = std::env::var("BUILDKITE_PULL_REQUEST_BASE_BRANCH") {
            if !base.is_empty() && base != "false" {
                return Some(base);
            }
        }

        // Fall back to pipeline default branch if available
        std::env::var("BUILDKITE_PIPELINE_DEFAULT_BRANCH").ok()
    }

    /// Fetch a ref if we're in a shallow clone.
    fn fetch_ref(refspec: &str) -> bool {
        debug!("Fetching ref: {refspec}");
        Command::new("git")
            .args(["fetch", "--depth=1", "origin", refspec])
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// Check if this is a shallow clone.
    fn is_shallow_clone() -> bool {
        Command::new("git")
            .args(["rev-parse", "--is-shallow-repository"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
    }

    /// Try to get changed files using git diff.
    fn try_git_diff(range: &str) -> Option<Vec<PathBuf>> {
        debug!("Trying git diff: {range}");
        let output = Command::new("git")
            .args(["diff", "--name-only", range])
            .output()
            .ok()?;

        if !output.status.success() {
            debug!(
                "git diff failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Some(
            stdout
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| PathBuf::from(line.trim()))
                .collect(),
        )
    }

    /// Get all tracked files as fallback.
    fn get_all_tracked_files() -> Vec<PathBuf> {
        Command::new("git")
            .args(["ls-files"])
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| PathBuf::from(line.trim()))
                    .collect()
            })
            .unwrap_or_default()
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
        let base_ref = Self::get_base_ref();

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
        })
    }

    fn context(&self) -> &CIContext {
        &self.context
    }

    async fn changed_files(&self) -> Result<Vec<PathBuf>> {
        let is_shallow = Self::is_shallow_clone();
        debug!("Shallow clone detected: {is_shallow}");

        // Strategy 1: Pull Request - use base_ref
        if let Some(base) = &self.context.base_ref {
            if !base.is_empty() {
                debug!("PR detected, base_ref: {base}");

                if is_shallow {
                    Self::fetch_ref(base);
                }

                if let Some(files) = Self::try_git_diff(&format!("origin/{base}...HEAD")) {
                    return Ok(files);
                }
            }
        }

        // Strategy 2: Compare against parent commit
        if let Some(files) = Self::try_git_diff("HEAD^..HEAD") {
            debug!("Using HEAD^ comparison");
            return Ok(files);
        }

        // Strategy 3: Fall back to all tracked files
        warn!(
            "Could not determine changed files (shallow clone: {is_shallow}). \
             Running all tasks. For better performance, ensure fetch-depth > 1."
        );

        Ok(Self::get_all_tracked_files())
    }

    async fn create_check(&self, name: &str) -> Result<CheckHandle> {
        // Buildkite uses annotations for status updates
        // Create an annotation with the check name
        let annotation_context = format!("cuenv-{}", name.replace(' ', "-").to_lowercase());

        info!("Creating Buildkite check annotation: {}", annotation_context);

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
        std::env::remove_var("BUILDKITE");
        assert!(BuildkiteCIProvider::detect().is_none());
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
            },
        );
    }
}
