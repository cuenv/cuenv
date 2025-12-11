use super::{CIContext, CIProvider};
use crate::report::{CheckHandle, PipelineReport, PipelineStatus, markdown::generate_summary};
use async_trait::async_trait;
use cuenv_core::Result;
use octocrab::Octocrab;
use std::path::PathBuf;
use std::process::Command;
use tracing::{debug, info, warn};

pub struct GitHubProvider {
    context: CIContext,
    token: String,
    owner: String,
    repo: String,
    run_id: Option<u64>,
    pr_number: Option<u64>,
}

const NULL_SHA: &str = "0000000000000000000000000000000000000000";

impl GitHubProvider {
    fn parse_repo(repo_str: &str) -> (String, String) {
        let parts: Vec<&str> = repo_str.split('/').collect();
        if parts.len() == 2 {
            (parts[0].to_string(), parts[1].to_string())
        } else {
            (String::new(), String::new())
        }
    }

    /// Extract PR number from GITHUB_REF (e.g., "refs/pull/123/merge" -> 123)
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
}

#[async_trait]
impl CIProvider for GitHubProvider {
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
            run_id: std::env::var("GITHUB_RUN_ID")
                .ok()
                .and_then(|s| s.parse().ok()),
            pr_number,
        })
    }

    fn context(&self) -> &CIContext {
        &self.context
    }

    async fn changed_files(&self) -> Result<Vec<PathBuf>> {
        let is_shallow = Self::is_shallow_clone();
        debug!("Shallow clone detected: {is_shallow}");

        // Strategy 1: Pull Request - use base_ref
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

        // Strategy 2: Push event with valid GITHUB_BEFORE
        if let Some(before_sha) = Self::get_before_sha() {
            debug!("Push event detected, GITHUB_BEFORE: {before_sha}");

            if is_shallow {
                Self::fetch_ref(&before_sha);
            }

            if let Some(files) = Self::try_git_diff(&format!("{before_sha}..HEAD")) {
                return Ok(files);
            }
        }

        // Strategy 3: Try comparing against parent commit
        if let Some(files) = Self::try_git_diff("HEAD^..HEAD") {
            debug!("Using HEAD^ comparison");
            return Ok(files);
        }

        // Strategy 4: Fall back to all tracked files
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
            PipelineStatus::Partial => octocrab::params::checks::CheckRunConclusion::Neutral,
            PipelineStatus::Pending => octocrab::params::checks::CheckRunConclusion::Neutral,
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

    #[test]
    fn test_parse_repo() {
        let (owner, repo) = GitHubProvider::parse_repo("cuenv/cuenv");
        assert_eq!(owner, "cuenv");
        assert_eq!(repo, "cuenv");
    }

    #[test]
    fn test_parse_repo_invalid() {
        let (owner, repo) = GitHubProvider::parse_repo("invalid");
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
            GitHubProvider::parse_pr_number("refs/pull/123/merge"),
            Some(123)
        );
        assert_eq!(
            GitHubProvider::parse_pr_number("refs/pull/456/head"),
            Some(456)
        );
        assert_eq!(GitHubProvider::parse_pr_number("refs/heads/main"), None);
        assert_eq!(GitHubProvider::parse_pr_number("main"), None);
    }
}
