use super::{CIContext, CIProvider};
use crate::report::{CheckHandle, PipelineReport};
use async_trait::async_trait;
use cuenv_core::Result;
use std::path::PathBuf;
use std::process::Command;

#[allow(dead_code)]
pub struct GitHubProvider {
    context: CIContext,
    token: String,
    owner: String,
    repo: String,
    run_id: Option<u64>,
}

impl GitHubProvider {
    fn parse_repo(repo_str: &str) -> (String, String) {
        let parts: Vec<&str> = repo_str.split('/').collect();
        if parts.len() == 2 {
            (parts[0].to_string(), parts[1].to_string())
        } else {
            (String::new(), String::new())
        }
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
        })
    }

    fn context(&self) -> &CIContext {
        &self.context
    }

    async fn changed_files(&self) -> Result<Vec<PathBuf>> {
        // Naive implementation using git CLI
        // In a real implementation, we'd want to be more robust about fetching logic
        let mut cmd = Command::new("git");
        cmd.arg("diff").arg("--name-only");

        if let Some(base) = &self.context.base_ref {
            // PR: origin/base...HEAD
            // We assume origin is the remote. This might be brittle.
            cmd.arg(format!("origin/{base}...HEAD"));
        } else {
            // Push: we need before/after from event payload strictly speaking,
            // but for now let's just look at HEAD^..HEAD or similar if we can't determine range.
            // Actually, for simple push, usually we want to see what changed in the pushed commits.
            // GitHub Actions usually does a shallow clone. We need to ensure fetch-depth: 0 is used.
            // For now, let's default to comparing against previous commit if no base ref.
            cmd.arg("HEAD^..HEAD");
        }

        let output = cmd.output().map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: None,
            operation: "git diff".to_string(),
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(cuenv_core::Error::Configuration {
                src: "git".to_string(),
                span: None,
                message: format!(
                    "Failed to detect changed files via git diff. If running in GitHub Actions, ensure 'fetch-depth: 0' is set in your workflow.\nGit error: {stderr}"
                ),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let files = stdout
            .lines()
            .map(|line| PathBuf::from(line.trim()))
            .collect();

        Ok(files)
    }

    async fn create_check(&self, _name: &str) -> Result<CheckHandle> {
        // TODO: Call GitHub API to create check run
        Ok(CheckHandle {
            id: "dummy-check-id".to_string(),
        })
    }

    async fn update_check(&self, _handle: &CheckHandle, _summary: &str) -> Result<()> {
        // TODO: Call GitHub API
        Ok(())
    }

    async fn complete_check(&self, _handle: &CheckHandle, _report: &PipelineReport) -> Result<()> {
        // TODO: Call GitHub API
        Ok(())
    }

    async fn upload_report(&self, _report: &PipelineReport) -> Result<Option<String>> {
        // TODO: Upload artifact
        Ok(None)
    }
}
