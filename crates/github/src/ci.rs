//! GitHub Actions CI provider.
//!
//! This provider integrates with GitHub Actions to:
//! - Detect changed files in PRs and pushes
//! - Create and update check runs
//! - Post PR comments with pipeline reports

use async_trait::async_trait;
use cuenv_ci::context::CIContext;
use cuenv_ci::provider::CIProvider;
use cuenv_ci::report::{CheckHandle, PipelineReport, PipelineStatus, markdown::generate_summary};
use cuenv_core::Result;
use octocrab::Octocrab;
use std::path::PathBuf;
use std::process::Command;
use tracing::{debug, info, warn};

/// GitHub Actions CI provider.
///
/// Provides CI integration for repositories hosted on GitHub using GitHub Actions.
pub struct GitHubCIProvider {
    context: CIContext,
    token: String,
    owner: String,
    repo: String,
    pr_number: Option<u64>,
}

const NULL_SHA: &str = "0000000000000000000000000000000000000000";

impl GitHubCIProvider {
    fn parse_repo(repo_str: &str) -> (String, String) {
        let parts: Vec<&str> = repo_str.split('/').collect();
        if parts.len() == 2 {
            (parts[0].to_string(), parts[1].to_string())
        } else {
            (String::new(), String::new())
        }
    }

    /// Extract PR number from `GITHUB_REF` (e.g., "refs/pull/123/merge" -> 123)
    fn parse_pr_number(github_ref: &str) -> Option<u64> {
        if github_ref.starts_with("refs/pull/") {
            github_ref
                .strip_prefix("refs/pull/")?
                .split('/')
                .next()?
                .parse()
                .ok()
        } else {
            None
        }
    }

    fn is_shallow_clone() -> bool {
        Command::new("git")
            .args(["rev-parse", "--is-shallow-repository"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
            .unwrap_or(false)
    }

    fn fetch_ref(refspec: &str) -> bool {
        debug!("Fetching ref: {refspec}");
        Command::new("git")
            .args(["fetch", "--depth=1", "origin", refspec])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn get_before_sha() -> Option<String> {
        std::env::var("GITHUB_BEFORE")
            .ok()
            .filter(|sha| sha != NULL_SHA && !sha.is_empty())
    }

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

    /// Create an octocrab instance authenticated with the GitHub token.
    fn octocrab(&self) -> Result<Octocrab> {
        if self.token.is_empty() {
            return Err(cuenv_core::Error::configuration(
                "GITHUB_TOKEN is not set or empty",
            ));
        }
        Octocrab::builder()
            .personal_token(self.token.clone())
            .build()
            .map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to create GitHub client: {e}"))
            })
    }

    /// Get changed files for a PR using the GitHub API.
    ///
    /// This is faster and more reliable than git diff for PRs, as it doesn't
    /// require fetching git history. Works with shallow clones.
    async fn get_pr_files_from_api(&self, pr_number: u64) -> Result<Vec<PathBuf>> {
        debug!("Fetching PR files from GitHub API for PR #{pr_number}");
        let octocrab = self.octocrab()?;

        let page = octocrab
            .pulls(&self.owner, &self.repo)
            .list_files(pr_number)
            .await
            .map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to get PR files from API: {e}"))
            })?;

        let files: Vec<PathBuf> = page
            .items
            .iter()
            .map(|f| PathBuf::from(&f.filename))
            .collect();

        info!("Got {} changed files from GitHub API", files.len());
        Ok(files)
    }
}

#[async_trait]
impl CIProvider for GitHubCIProvider {
    fn detect() -> Option<Self> {
        if std::env::var("GITHUB_ACTIONS").ok()? != "true" {
            return None;
        }

        let repo_str = std::env::var("GITHUB_REPOSITORY").ok()?;
        let (owner, repo) = Self::parse_repo(&repo_str);

        let github_ref = std::env::var("GITHUB_REF").unwrap_or_default();
        let pr_number = Self::parse_pr_number(&github_ref);

        Some(Self {
            context: CIContext {
                provider: "github".to_string(),
                event: std::env::var("GITHUB_EVENT_NAME").unwrap_or_default(),
                ref_name: std::env::var("GITHUB_REF_NAME").unwrap_or_default(),
                base_ref: std::env::var("GITHUB_BASE_REF").ok(),
                sha: std::env::var("GITHUB_SHA").unwrap_or_default(),
            },
            token: std::env::var("GITHUB_TOKEN").unwrap_or_default(),
            owner,
            repo,
            pr_number,
        })
    }

    fn context(&self) -> &CIContext {
        &self.context
    }

    async fn changed_files(&self) -> Result<Vec<PathBuf>> {
        // Strategy 1: Pull Request - use GitHub API (fastest, no git history needed)
        if let Some(pr_number) = self.pr_number {
            debug!("PR #{pr_number} detected, using GitHub API for changed files");
            match self.get_pr_files_from_api(pr_number).await {
                Ok(files) => return Ok(files),
                Err(e) => {
                    warn!("Failed to get PR files from API: {e}. Falling back to git diff.");
                }
            }
        }

        let is_shallow = Self::is_shallow_clone();
        debug!("Shallow clone detected: {is_shallow}");

        // Strategy 2: Pull Request - use git diff with base_ref (fallback if API fails)
        if let Some(base) = &self.context.base_ref
            && !base.is_empty()
        {
            debug!("PR detected, base_ref: {base}");

            if is_shallow {
                Self::fetch_ref(base);
            }

            if let Some(files) = Self::try_git_diff(&format!("origin/{base}...HEAD")) {
                return Ok(files);
            }
        }

        // Strategy 3: Push event with valid GITHUB_BEFORE
        if let Some(before_sha) = Self::get_before_sha() {
            debug!("Push event detected, GITHUB_BEFORE: {before_sha}");

            if is_shallow {
                Self::fetch_ref(&before_sha);
            }

            if let Some(files) = Self::try_git_diff(&format!("{before_sha}..HEAD")) {
                return Ok(files);
            }
        }

        // Strategy 4: Try comparing against parent commit
        if let Some(files) = Self::try_git_diff("HEAD^..HEAD") {
            debug!("Using HEAD^ comparison");
            return Ok(files);
        }

        // Strategy 5: Fall back to all tracked files
        warn!(
            "Could not determine changed files (shallow clone: {is_shallow}). \
             Running all tasks. For better performance, consider: \
             1) Set 'fetch-depth: 2' for push events, or \
             2) This may be a new branch with no history to compare."
        );

        Ok(Self::get_all_tracked_files())
    }

    async fn create_check(&self, name: &str) -> Result<CheckHandle> {
        let octocrab = self.octocrab()?;

        let check_run = octocrab
            .checks(&self.owner, &self.repo)
            .create_check_run(name, &self.context.sha)
            .status(octocrab::params::checks::CheckRunStatus::InProgress)
            .send()
            .await
            .map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to create check run: {e}"))
            })?;

        info!("Created check run: {} (id: {})", name, check_run.id);

        Ok(CheckHandle {
            id: check_run.id.to_string(),
        })
    }

    async fn update_check(&self, handle: &CheckHandle, summary: &str) -> Result<()> {
        let octocrab = self.octocrab()?;
        let check_run_id: u64 = handle
            .id
            .parse()
            .map_err(|_| cuenv_core::Error::configuration("Invalid check run ID"))?;

        octocrab
            .checks(&self.owner, &self.repo)
            .update_check_run(check_run_id.into())
            .output(octocrab::params::checks::CheckRunOutput {
                title: "cuenv CI".to_string(),
                summary: summary.to_string(),
                text: None,
                annotations: vec![],
                images: vec![],
            })
            .send()
            .await
            .map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to update check run: {e}"))
            })?;

        Ok(())
    }

    async fn complete_check(&self, handle: &CheckHandle, report: &PipelineReport) -> Result<()> {
        let octocrab = self.octocrab()?;
        let check_run_id: u64 = handle
            .id
            .parse()
            .map_err(|_| cuenv_core::Error::configuration("Invalid check run ID"))?;

        let conclusion = match report.status {
            PipelineStatus::Success => octocrab::params::checks::CheckRunConclusion::Success,
            PipelineStatus::Failed => octocrab::params::checks::CheckRunConclusion::Failure,
            PipelineStatus::Partial | PipelineStatus::Pending => {
                octocrab::params::checks::CheckRunConclusion::Neutral
            }
        };

        let summary = generate_summary(report);

        octocrab
            .checks(&self.owner, &self.repo)
            .update_check_run(check_run_id.into())
            .status(octocrab::params::checks::CheckRunStatus::Completed)
            .conclusion(conclusion)
            .output(octocrab::params::checks::CheckRunOutput {
                title: format!("cuenv: {}", report.project),
                summary,
                text: None,
                annotations: vec![],
                images: vec![],
            })
            .send()
            .await
            .map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to complete check run: {e}"))
            })?;

        info!("Completed check run: {}", handle.id);

        Ok(())
    }

    async fn upload_report(&self, report: &PipelineReport) -> Result<Option<String>> {
        // Only post PR comments for pull_request events
        let Some(pr_number) = self.pr_number else {
            debug!("Not a PR event, skipping PR comment");
            return Ok(None);
        };

        let octocrab = self.octocrab()?;
        let summary = generate_summary(report);

        let comment = octocrab
            .issues(&self.owner, &self.repo)
            .create_comment(pr_number, &summary)
            .await
            .map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to post PR comment: {e}"))
            })?;

        info!("Posted PR comment: {}", comment.html_url);

        Ok(Some(comment.html_url.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn test_parse_repo() {
        let (owner, repo) = GitHubCIProvider::parse_repo("cuenv/cuenv");
        assert_eq!(owner, "cuenv");
        assert_eq!(repo, "cuenv");
    }

    #[test]
    fn test_parse_repo_different_names() {
        let (owner, repo) = GitHubCIProvider::parse_repo("organization/project-name");
        assert_eq!(owner, "organization");
        assert_eq!(repo, "project-name");
    }

    #[test]
    fn test_parse_repo_invalid() {
        let (owner, repo) = GitHubCIProvider::parse_repo("invalid");
        assert_eq!(owner, "");
        assert_eq!(repo, "");
    }

    #[test]
    fn test_parse_repo_empty() {
        let (owner, repo) = GitHubCIProvider::parse_repo("");
        assert_eq!(owner, "");
        assert_eq!(repo, "");
    }

    #[test]
    fn test_parse_repo_too_many_parts() {
        let (owner, repo) = GitHubCIProvider::parse_repo("a/b/c/d");
        assert_eq!(owner, "");
        assert_eq!(repo, "");
    }

    #[test]
    fn test_null_sha_constant() {
        assert_eq!(NULL_SHA.len(), 40);
        assert!(NULL_SHA.chars().all(|c| c == '0'));
    }

    #[test]
    fn test_parse_pr_number() {
        assert_eq!(
            GitHubCIProvider::parse_pr_number("refs/pull/123/merge"),
            Some(123)
        );
        assert_eq!(
            GitHubCIProvider::parse_pr_number("refs/pull/456/head"),
            Some(456)
        );
        assert_eq!(GitHubCIProvider::parse_pr_number("refs/heads/main"), None);
        assert_eq!(GitHubCIProvider::parse_pr_number("main"), None);
    }

    #[test]
    fn test_parse_pr_number_large() {
        assert_eq!(
            GitHubCIProvider::parse_pr_number("refs/pull/999999/merge"),
            Some(999_999)
        );
    }

    #[test]
    fn test_parse_pr_number_zero() {
        // Edge case: PR number 0 (unlikely but possible in parsing)
        assert_eq!(
            GitHubCIProvider::parse_pr_number("refs/pull/0/merge"),
            Some(0)
        );
    }

    #[test]
    fn test_parse_pr_number_empty() {
        assert_eq!(GitHubCIProvider::parse_pr_number(""), None);
    }

    #[test]
    fn test_parse_pr_number_refs_pull_only() {
        assert_eq!(GitHubCIProvider::parse_pr_number("refs/pull/"), None);
    }

    #[test]
    fn test_parse_pr_number_invalid_number() {
        assert_eq!(
            GitHubCIProvider::parse_pr_number("refs/pull/abc/merge"),
            None
        );
    }

    #[test]
    fn test_parse_pr_number_branch_ref() {
        // Typical branch refs should return None
        assert_eq!(
            GitHubCIProvider::parse_pr_number("refs/heads/feature/test"),
            None
        );
        assert_eq!(
            GitHubCIProvider::parse_pr_number("refs/heads/develop"),
            None
        );
        assert_eq!(GitHubCIProvider::parse_pr_number("refs/tags/v1.0.0"), None);
    }

    #[test]
    #[serial]
    #[allow(unsafe_code)]
    fn test_get_before_sha_filters_null_sha() {
        // The NULL_SHA should be filtered out
        // SAFETY: Test runs in isolation
        unsafe {
            std::env::set_var("GITHUB_BEFORE", NULL_SHA);
        }
        assert!(GitHubCIProvider::get_before_sha().is_none());

        // Clean up
        unsafe {
            std::env::remove_var("GITHUB_BEFORE");
        }
    }

    #[test]
    #[serial]
    #[allow(unsafe_code)]
    fn test_get_before_sha_filters_empty() {
        // Empty string should be filtered out
        // SAFETY: Test runs in isolation
        unsafe {
            std::env::set_var("GITHUB_BEFORE", "");
        }
        assert!(GitHubCIProvider::get_before_sha().is_none());

        // Clean up
        unsafe {
            std::env::remove_var("GITHUB_BEFORE");
        }
    }

    #[test]
    #[serial]
    #[allow(unsafe_code)]
    fn test_get_before_sha_valid() {
        // A valid SHA should be returned
        let valid_sha = "abc123def456";
        // SAFETY: Test runs in isolation
        unsafe {
            std::env::set_var("GITHUB_BEFORE", valid_sha);
        }
        assert_eq!(
            GitHubCIProvider::get_before_sha(),
            Some(valid_sha.to_string())
        );

        // Clean up
        unsafe {
            std::env::remove_var("GITHUB_BEFORE");
        }
    }

    #[test]
    #[serial]
    #[allow(unsafe_code)]
    fn test_detect_not_github_actions() {
        // Clear GitHub Actions environment variables
        // SAFETY: Test runs in isolation
        unsafe {
            std::env::remove_var("GITHUB_ACTIONS");
            std::env::remove_var("GITHUB_REPOSITORY");
        }

        let provider = GitHubCIProvider::detect();
        assert!(provider.is_none());
    }

    #[test]
    #[serial]
    #[allow(unsafe_code)]
    fn test_detect_github_actions_false() {
        // SAFETY: Test runs in isolation
        unsafe {
            std::env::set_var("GITHUB_ACTIONS", "false");
        }

        let provider = GitHubCIProvider::detect();
        assert!(provider.is_none());

        // Clean up
        unsafe {
            std::env::remove_var("GITHUB_ACTIONS");
        }
    }

    #[test]
    fn test_try_git_diff_parses_output() {
        // This test just verifies the diff output parsing logic
        // In a real repo, this would test actual git diff output

        // Test that empty output results in empty vec
        // This is implicitly tested through the filter logic
        let empty_lines = "";
        let files: Vec<PathBuf> = empty_lines
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| PathBuf::from(line.trim()))
            .collect();
        assert!(files.is_empty());

        // Test that whitespace-only lines are filtered
        let whitespace_only = "   \n\t\n";
        let files: Vec<PathBuf> = whitespace_only
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| PathBuf::from(line.trim()))
            .collect();
        assert!(files.is_empty());

        // Test that valid file paths are parsed
        let valid_output = "src/main.rs\nCargo.toml\nREADME.md";
        let files: Vec<PathBuf> = valid_output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| PathBuf::from(line.trim()))
            .collect();
        assert_eq!(files.len(), 3);
        assert_eq!(files[0], PathBuf::from("src/main.rs"));
        assert_eq!(files[1], PathBuf::from("Cargo.toml"));
        assert_eq!(files[2], PathBuf::from("README.md"));
    }
}
