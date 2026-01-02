//! GitHub Releases backend for cuenv.
//!
//! Implements the [`ReleaseBackend`] trait for uploading release artifacts
//! to GitHub Releases.

use bytes::Bytes;
use cuenv_release::artifact::PackagedArtifact;
use cuenv_release::backends::{BackendContext, PublishResult, ReleaseBackend};
use cuenv_release::error::Result;
use octocrab::Octocrab;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use tracing::{debug, info};

/// Configuration for the GitHub Releases backend.
#[derive(Debug, Clone)]
pub struct GitHubReleaseConfig {
    /// Repository owner (e.g., "cuenv")
    pub owner: String,
    /// Repository name (e.g., "cuenv")
    pub repo: String,
    /// GitHub token for authentication
    pub token: String,
    /// Whether to create the release as a draft
    pub draft: bool,
    /// Whether to mark the release as a prerelease
    pub prerelease: bool,
}

impl GitHubReleaseConfig {
    /// Creates a new GitHub release configuration.
    #[must_use]
    pub fn new(
        owner: impl Into<String>,
        repo: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
            token: token.into(),
            draft: false,
            prerelease: false,
        }
    }

    /// Sets the draft flag.
    #[must_use]
    pub const fn with_draft(mut self, draft: bool) -> Self {
        self.draft = draft;
        self
    }

    /// Sets the prerelease flag.
    #[must_use]
    pub const fn with_prerelease(mut self, prerelease: bool) -> Self {
        self.prerelease = prerelease;
        self
    }

    /// Creates configuration from environment and git remote.
    ///
    /// Reads `GITHUB_TOKEN` from environment and parses owner/repo from
    /// the provided remote URL.
    #[must_use]
    pub fn from_env(remote_url: &str) -> Option<Self> {
        let token = std::env::var("GITHUB_TOKEN").ok()?;
        let (owner, repo) = parse_github_remote(remote_url)?;
        Some(Self::new(owner, repo, token))
    }
}

/// Parse a GitHub remote URL into (owner, repo).
fn parse_github_remote(url: &str) -> Option<(String, String)> {
    // Handle SSH format: git@github.com:owner/repo.git
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        let path = rest.strip_suffix(".git").unwrap_or(rest);
        let (owner, repo) = path.split_once('/')?;
        return Some((owner.to_string(), repo.to_string()));
    }

    // Handle HTTPS format: https://github.com/owner/repo.git
    if let Some(rest) = url.strip_prefix("https://github.com/") {
        let path = rest.strip_suffix(".git").unwrap_or(rest);
        let (owner, repo) = path.split_once('/')?;
        return Some((owner.to_string(), repo.to_string()));
    }

    None
}

/// GitHub Releases backend.
///
/// Uploads release artifacts to GitHub Releases. Handles:
/// - Finding or creating the release for the given version
/// - Uploading tarball assets
/// - Uploading checksum files
pub struct GitHubReleaseBackend {
    config: GitHubReleaseConfig,
}

impl GitHubReleaseBackend {
    /// Creates a new GitHub release backend.
    #[must_use]
    pub const fn new(config: GitHubReleaseConfig) -> Self {
        Self { config }
    }

    /// Creates an authenticated Octocrab client.
    fn client(&self) -> Result<Octocrab> {
        Octocrab::builder()
            .personal_token(self.config.token.clone())
            .build()
            .map_err(|e| cuenv_release::error::Error::backend("github", e.to_string(), None))
    }

    /// Finds an existing release by tag name.
    async fn find_release(&self, client: &Octocrab, tag: &str) -> Result<Option<u64>> {
        let repos = client.repos(&self.config.owner, &self.config.repo);
        let releases = repos.releases();

        match releases.get_by_tag(tag).await {
            Ok(release) => Ok(Some(release.id.0)),
            Err(octocrab::Error::GitHub { source, .. }) if source.message.contains("Not Found") => {
                Ok(None)
            }
            Err(e) => Err(cuenv_release::error::Error::backend(
                "github",
                e.to_string(),
                None,
            )),
        }
    }

    /// Creates a new release.
    async fn create_release(&self, client: &Octocrab, ctx: &BackendContext) -> Result<u64> {
        let tag = format!("v{}", ctx.version);
        let repos = client.repos(&self.config.owner, &self.config.repo);
        let releases = repos.releases();

        let release = releases
            .create(&tag)
            .name(&format!("{} v{}", ctx.name, ctx.version))
            .draft(self.config.draft)
            .prerelease(self.config.prerelease)
            .send()
            .await
            .map_err(|e| cuenv_release::error::Error::backend("github", e.to_string(), None))?;

        Ok(release.id.0)
    }

    /// Uploads an asset to a release.
    async fn upload_asset(
        &self,
        client: &Octocrab,
        release_id: u64,
        path: &Path,
        name: &str,
    ) -> Result<String> {
        let data = tokio::fs::read(path).await.map_err(|e| {
            cuenv_release::error::Error::artifact(e.to_string(), Some(path.to_path_buf()))
        })?;

        let repos = client.repos(&self.config.owner, &self.config.repo);
        let releases = repos.releases();

        let asset = releases
            .upload_asset(release_id, name, Bytes::from(data))
            .send()
            .await
            .map_err(|e| cuenv_release::error::Error::backend("github", e.to_string(), None))?;

        Ok(asset.browser_download_url.to_string())
    }
}

impl ReleaseBackend for GitHubReleaseBackend {
    fn name(&self) -> &'static str {
        "GitHub Releases"
    }

    fn publish<'a>(
        &'a self,
        ctx: &'a BackendContext,
        artifacts: &'a [PackagedArtifact],
    ) -> Pin<Box<dyn Future<Output = Result<PublishResult>> + Send + 'a>> {
        Box::pin(async move {
            let tag = format!("v{}", ctx.version);

            if ctx.dry_run {
                info!(
                    owner = %self.config.owner,
                    repo = %self.config.repo,
                    tag = %tag,
                    artifact_count = artifacts.len(),
                    "Would upload artifacts to GitHub release"
                );
                return Ok(PublishResult::dry_run(
                    "GitHub Releases",
                    format!(
                        "Would upload {} artifacts to {}/{} release {}",
                        artifacts.len(),
                        self.config.owner,
                        self.config.repo,
                        tag
                    ),
                ));
            }

            let client = self.client()?;

            // Find or create release
            let release_id = if let Some(id) = self.find_release(&client, &tag).await? {
                debug!(release_id = id, tag = %tag, "Found existing release");
                id
            } else {
                info!(tag = %tag, "Creating new release");
                self.create_release(&client, ctx).await?
            };

            // Upload each artifact
            let mut uploaded = Vec::new();
            for artifact in artifacts {
                debug!(
                    archive = %artifact.archive_name,
                    target = ?artifact.target,
                    "Uploading artifact"
                );

                let url = self
                    .upload_asset(
                        &client,
                        release_id,
                        &artifact.archive_path,
                        &artifact.archive_name,
                    )
                    .await?;

                uploaded.push(url);
            }

            // Upload checksums file if present
            if let Some(first) = artifacts.first() {
                let checksums_path = first.archive_path.parent().map(|p| p.join("CHECKSUMS.txt"));
                if let Some(path) = checksums_path.filter(|p| p.exists()) {
                    debug!("Uploading CHECKSUMS.txt");
                    self.upload_asset(&client, release_id, &path, "CHECKSUMS.txt")
                        .await?;
                }
            }

            let release_url = format!(
                "https://github.com/{}/{}/releases/tag/{}",
                self.config.owner, self.config.repo, tag
            );

            info!(
                release_url = %release_url,
                uploaded_count = uploaded.len(),
                "Published to GitHub Releases"
            );

            Ok(PublishResult::success_with_url(
                "GitHub Releases",
                format!("Uploaded {} artifacts", uploaded.len()),
                release_url,
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_github_remote_ssh() {
        let result = parse_github_remote("git@github.com:cuenv/cuenv.git");
        assert_eq!(result, Some(("cuenv".to_string(), "cuenv".to_string())));
    }

    #[test]
    fn test_parse_github_remote_ssh_no_git_suffix() {
        let result = parse_github_remote("git@github.com:owner/repo");
        assert_eq!(result, Some(("owner".to_string(), "repo".to_string())));
    }

    #[test]
    fn test_parse_github_remote_https() {
        let result = parse_github_remote("https://github.com/cuenv/cuenv.git");
        assert_eq!(result, Some(("cuenv".to_string(), "cuenv".to_string())));
    }

    #[test]
    fn test_parse_github_remote_https_no_git_suffix() {
        let result = parse_github_remote("https://github.com/owner/repo");
        assert_eq!(result, Some(("owner".to_string(), "repo".to_string())));
    }

    #[test]
    fn test_parse_github_remote_invalid() {
        assert!(parse_github_remote("https://gitlab.com/owner/repo").is_none());
        assert!(parse_github_remote("not a url").is_none());
    }

    #[test]
    fn test_parse_github_remote_bitbucket() {
        // Non-GitHub remotes should return None
        assert!(parse_github_remote("git@bitbucket.org:owner/repo.git").is_none());
        assert!(parse_github_remote("https://bitbucket.org/owner/repo.git").is_none());
    }

    #[test]
    fn test_parse_github_remote_empty() {
        assert!(parse_github_remote("").is_none());
    }

    #[test]
    fn test_parse_github_remote_partial_url() {
        // Missing repo part
        assert!(parse_github_remote("https://github.com/owner").is_none());
    }

    #[test]
    fn test_parse_github_remote_nested_path() {
        // Only owner/repo is valid, deeper paths should still parse the first two parts
        let result = parse_github_remote("https://github.com/owner/repo/extra/path");
        // This will only get owner/repo/extra/path as the second part
        // and then split_once('/') gives "repo/extra/path" as repo
        assert!(result.is_some());
        let (owner, repo) = result.unwrap();
        assert_eq!(owner, "owner");
        // The repo will include the extra path parts
        assert!(repo.starts_with("repo"));
    }

    #[test]
    fn test_config_builder() {
        let config = GitHubReleaseConfig::new("owner", "repo", "token")
            .with_draft(true)
            .with_prerelease(true);

        assert_eq!(config.owner, "owner");
        assert_eq!(config.repo, "repo");
        assert!(config.draft);
        assert!(config.prerelease);
    }

    #[test]
    fn test_config_defaults() {
        let config = GitHubReleaseConfig::new("owner", "repo", "token");

        assert_eq!(config.owner, "owner");
        assert_eq!(config.repo, "repo");
        assert_eq!(config.token, "token");
        // Defaults should be false
        assert!(!config.draft);
        assert!(!config.prerelease);
    }

    #[test]
    fn test_config_with_draft_only() {
        let config = GitHubReleaseConfig::new("owner", "repo", "token").with_draft(true);

        assert!(config.draft);
        assert!(!config.prerelease);
    }

    #[test]
    fn test_config_with_prerelease_only() {
        let config = GitHubReleaseConfig::new("owner", "repo", "token").with_prerelease(true);

        assert!(!config.draft);
        assert!(config.prerelease);
    }

    #[test]
    fn test_config_clone() {
        let config = GitHubReleaseConfig::new("owner", "repo", "token")
            .with_draft(true)
            .with_prerelease(true);

        let cloned = config.clone();
        assert_eq!(cloned.owner, config.owner);
        assert_eq!(cloned.repo, config.repo);
        assert_eq!(cloned.token, config.token);
        assert_eq!(cloned.draft, config.draft);
        assert_eq!(cloned.prerelease, config.prerelease);
    }

    #[test]
    fn test_config_debug() {
        let config = GitHubReleaseConfig::new("owner", "repo", "token");
        let debug_str = format!("{:?}", config);

        // Debug output should contain the field names
        assert!(debug_str.contains("owner"));
        assert!(debug_str.contains("repo"));
    }

    #[test]
    fn test_backend_new() {
        let config = GitHubReleaseConfig::new("owner", "repo", "token");
        let backend = GitHubReleaseBackend::new(config);

        assert_eq!(backend.name(), "GitHub Releases");
    }

    #[test]
    fn test_backend_name() {
        let config = GitHubReleaseConfig::new("owner", "repo", "token");
        let backend = GitHubReleaseBackend::new(config);

        assert_eq!(backend.name(), "GitHub Releases");
    }

    #[test]
    #[allow(unsafe_code)]
    fn test_from_env_no_token() {
        // Clear any existing GITHUB_TOKEN
        // SAFETY: This test should run in isolation
        unsafe {
            std::env::remove_var("GITHUB_TOKEN");
        }

        let result = GitHubReleaseConfig::from_env("git@github.com:owner/repo.git");
        assert!(result.is_none());
    }

    #[test]
    #[allow(unsafe_code)]
    fn test_from_env_invalid_url() {
        // SAFETY: This test should run in isolation
        unsafe {
            std::env::set_var("GITHUB_TOKEN", "test_token");
        }

        let result = GitHubReleaseConfig::from_env("not a valid url");
        assert!(result.is_none());

        // Clean up
        unsafe {
            std::env::remove_var("GITHUB_TOKEN");
        }
    }
}
