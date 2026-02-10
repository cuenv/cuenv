//! Homebrew backend for cuenv releases.
//!
//! Generates and pushes Homebrew formulas to a tap repository.

use crate::formula::{BinaryInfo, FormulaData, FormulaGenerator};
use cuenv_release::PackagedArtifact;
use cuenv_release::backends::{BackendContext, PublishResult, ReleaseBackend};
use cuenv_release::error::Result;
use octocrab::Octocrab;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use tracing::{debug, info};

/// Configuration for the Homebrew backend.
#[derive(Debug, Clone)]
pub struct HomebrewConfig {
    /// Tap repository in "owner/repo" format (e.g., "cuenv/homebrew-tap")
    pub tap: String,
    /// Formula name (defaults to project name)
    pub formula: String,
    /// License identifier (e.g., "AGPL-3.0-or-later")
    pub license: String,
    /// Project homepage URL
    pub homepage: String,
    /// GitHub token for pushing to tap (reads from env if not set)
    pub token: Option<String>,
    /// Token environment variable name (default: `HOMEBREW_TAP_TOKEN`)
    pub token_env: String,
}

impl HomebrewConfig {
    /// Creates a new Homebrew configuration.
    #[must_use]
    pub fn new(tap: impl Into<String>, formula: impl Into<String>) -> Self {
        Self {
            tap: tap.into(),
            formula: formula.into(),
            license: String::new(),
            homepage: String::new(),
            token: None,
            token_env: "HOMEBREW_TAP_TOKEN".to_string(),
        }
    }

    /// Sets the license.
    #[must_use]
    pub fn with_license(mut self, license: impl Into<String>) -> Self {
        self.license = license.into();
        self
    }

    /// Sets the homepage.
    #[must_use]
    pub fn with_homepage(mut self, homepage: impl Into<String>) -> Self {
        self.homepage = homepage.into();
        self
    }

    /// Sets the GitHub token directly.
    #[must_use]
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Sets the token environment variable name.
    #[must_use]
    pub fn with_token_env(mut self, env_var: impl Into<String>) -> Self {
        self.token_env = env_var.into();
        self
    }

    /// Gets the token, either from config or environment.
    fn get_token(&self) -> Option<String> {
        self.token
            .clone()
            .or_else(|| std::env::var(&self.token_env).ok())
    }
}

/// Parse a tap string into (owner, repo).
fn parse_tap(tap: &str) -> Option<(String, String)> {
    let (owner, repo) = tap.split_once('/')?;
    Some((owner.to_string(), repo.to_string()))
}

/// Homebrew backend for updating tap repositories.
pub struct HomebrewBackend {
    config: HomebrewConfig,
}

impl HomebrewBackend {
    /// Creates a new Homebrew backend.
    #[must_use]
    pub const fn new(config: HomebrewConfig) -> Self {
        Self { config }
    }

    /// Creates formula data from packaged artifacts.
    fn create_formula_data(
        &self,
        ctx: &BackendContext,
        artifacts: &[PackagedArtifact],
    ) -> FormulaData {
        let mut binaries = HashMap::new();

        let base_url = ctx
            .download_base_url
            .as_deref()
            .unwrap_or("https://github.com/OWNER/REPO/releases/download");

        for artifact in artifacts {
            let url = format!("{}/v{}/{}", base_url, ctx.version, artifact.archive_name);
            binaries.insert(
                artifact.target,
                BinaryInfo {
                    url,
                    sha256: artifact.sha256.clone(),
                },
            );
        }

        // Convert formula name to class name (capitalize first letter)
        let class_name = self
            .config
            .formula
            .chars()
            .enumerate()
            .map(|(i, c)| if i == 0 { c.to_ascii_uppercase() } else { c })
            .collect();

        FormulaData {
            class_name,
            desc: format!("{} - environment management tool", ctx.name),
            homepage: self.config.homepage.clone(),
            license: self.config.license.clone(),
            version: ctx.version.clone(),
            binaries,
        }
    }

    /// Generates the formula content without publishing.
    #[must_use]
    pub fn generate_formula(&self, ctx: &BackendContext, artifacts: &[PackagedArtifact]) -> String {
        let data = self.create_formula_data(ctx, artifacts);
        FormulaGenerator::generate(&data)
    }

    /// Creates an authenticated Octocrab client.
    fn client(&self) -> Result<Octocrab> {
        let token = self.config.get_token().ok_or_else(|| {
            cuenv_release::error::Error::backend(
                "Homebrew",
                format!(
                    "No token found. Set {} environment variable or provide token in config",
                    self.config.token_env
                ),
                Some(format!(
                    "export {}=<your-github-token>",
                    self.config.token_env
                )),
            )
        })?;

        Octocrab::builder()
            .personal_token(token)
            .build()
            .map_err(|e| cuenv_release::error::Error::backend("Homebrew", e.to_string(), None))
    }

    /// Pushes the formula to the tap repository.
    async fn push_formula(
        &self,
        client: &Octocrab,
        formula_content: &str,
        version: &str,
    ) -> Result<String> {
        let (owner, repo) = parse_tap(&self.config.tap).ok_or_else(|| {
            cuenv_release::error::Error::backend(
                "Homebrew",
                format!(
                    "Invalid tap format: '{}'. Expected 'owner/repo'",
                    self.config.tap
                ),
                None,
            )
        })?;

        let path = format!("Formula/{}.rb", self.config.formula);
        let commit_message = format!("Update {} to {}", self.config.formula, version);

        debug!(
            owner = %owner,
            repo = %repo,
            path = %path,
            "Pushing formula to tap"
        );

        // Try to get existing file to get its SHA (needed for updates)
        let repos = client.repos(&owner, &repo);
        let existing_sha = match repos.get_content().path(&path).send().await {
            Ok(content) => content.items.first().map(|item| item.sha.clone()),
            Err(_) => None,
        };

        // Encode content as base64
        let encoded_content = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            formula_content.as_bytes(),
        );

        // Create or update the file
        let result = if let Some(sha) = existing_sha {
            debug!(sha = %sha, "Updating existing formula");
            repos
                .update_file(&path, &commit_message, &encoded_content, &sha)
                .branch("main")
                .send()
                .await
        } else {
            debug!("Creating new formula");
            repos
                .create_file(&path, &commit_message, &encoded_content)
                .branch("main")
                .send()
                .await
        };

        result
            .map_err(|e| cuenv_release::error::Error::backend("Homebrew", e.to_string(), None))?;

        let formula_url = format!("https://github.com/{owner}/{repo}/blob/main/{path}");

        info!(url = %formula_url, "Formula pushed to tap");

        Ok(formula_url)
    }
}

impl ReleaseBackend for HomebrewBackend {
    fn name(&self) -> &'static str {
        "Homebrew"
    }

    fn publish<'a>(
        &'a self,
        ctx: &'a BackendContext,
        artifacts: &'a [PackagedArtifact],
    ) -> Pin<Box<dyn Future<Output = Result<PublishResult>> + Send + 'a>> {
        Box::pin(async move {
            let formula_content = self.generate_formula(ctx, artifacts);

            debug!(
                formula_name = %self.config.formula,
                formula_len = formula_content.len(),
                "Generated formula"
            );

            if ctx.dry_run.is_dry_run() {
                info!(
                    tap = %self.config.tap,
                    formula = %self.config.formula,
                    "Would update Homebrew formula"
                );
                return Ok(PublishResult::dry_run(
                    "Homebrew",
                    format!(
                        "Would update formula {} in {}",
                        self.config.formula, self.config.tap
                    ),
                ));
            }

            let client = self.client()?;
            let formula_url = self
                .push_formula(&client, &formula_content, &ctx.version)
                .await?;

            Ok(PublishResult::success_with_url(
                "Homebrew",
                format!(
                    "Updated formula {} in {}",
                    self.config.formula, self.config.tap
                ),
                formula_url,
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_release::{DryRun, Target};

    #[test]
    fn test_parse_tap() {
        let result = parse_tap("cuenv/homebrew-tap");
        assert_eq!(
            result,
            Some(("cuenv".to_string(), "homebrew-tap".to_string()))
        );
    }

    #[test]
    fn test_parse_tap_invalid() {
        assert!(parse_tap("invalid").is_none());
        assert!(parse_tap("").is_none());
    }

    #[test]
    fn test_config_builder() {
        let config = HomebrewConfig::new("owner/tap", "formula")
            .with_license("MIT")
            .with_homepage("https://example.com")
            .with_token_env("MY_TOKEN");

        assert_eq!(config.tap, "owner/tap");
        assert_eq!(config.formula, "formula");
        assert_eq!(config.license, "MIT");
        assert_eq!(config.homepage, "https://example.com");
        assert_eq!(config.token_env, "MY_TOKEN");
    }

    #[test]
    fn test_config_new_defaults() {
        let config = HomebrewConfig::new("owner/tap", "myformula");
        assert_eq!(config.tap, "owner/tap");
        assert_eq!(config.formula, "myformula");
        assert!(config.license.is_empty());
        assert!(config.homepage.is_empty());
        assert!(config.token.is_none());
        assert_eq!(config.token_env, "HOMEBREW_TAP_TOKEN");
    }

    #[test]
    fn test_config_with_token() {
        let config = HomebrewConfig::new("owner/tap", "formula").with_token("my-secret-token");
        assert_eq!(config.token, Some("my-secret-token".to_string()));
    }

    #[test]
    fn test_config_clone() {
        let config = HomebrewConfig::new("owner/tap", "formula")
            .with_license("MIT")
            .with_homepage("https://example.com");
        let cloned = config.clone();
        assert_eq!(config.tap, cloned.tap);
        assert_eq!(config.formula, cloned.formula);
        assert_eq!(config.license, cloned.license);
    }

    #[test]
    fn test_config_debug() {
        let config = HomebrewConfig::new("owner/tap", "formula");
        let debug_str = format!("{config:?}");
        assert!(debug_str.contains("HomebrewConfig"));
        assert!(debug_str.contains("owner/tap"));
        assert!(debug_str.contains("formula"));
    }

    #[test]
    fn test_parse_tap_multiple_slashes() {
        let result = parse_tap("owner/repo/extra");
        // Should only split on first slash
        assert_eq!(
            result,
            Some(("owner".to_string(), "repo/extra".to_string()))
        );
    }

    #[test]
    fn test_parse_tap_org_with_dash() {
        let result = parse_tap("my-org/homebrew-formulas");
        assert_eq!(
            result,
            Some(("my-org".to_string(), "homebrew-formulas".to_string()))
        );
    }

    #[test]
    fn test_backend_name() {
        let config = HomebrewConfig::new("owner/tap", "formula");
        let backend = HomebrewBackend::new(config);
        assert_eq!(backend.name(), "Homebrew");
    }

    #[test]
    fn test_generate_formula_with_artifacts() {
        let config = HomebrewConfig::new("owner/tap", "cuenv")
            .with_license("AGPL-3.0")
            .with_homepage("https://cuenv.io");
        let backend = HomebrewBackend::new(config);

        let ctx = BackendContext {
            name: "cuenv".to_string(),
            version: "1.0.0".to_string(),
            download_base_url: Some("https://github.com/cuenv/cuenv/releases/download".to_string()),
            dry_run: DryRun::No,
        };

        let artifacts = vec![PackagedArtifact {
            target: Target::DarwinArm64,
            archive_name: "cuenv-darwin-arm64.tar.gz".to_string(),
            sha256: "abcdef123456".to_string(),
            archive_path: std::path::PathBuf::from("/tmp/cuenv-darwin-arm64.tar.gz"),
            checksum_path: std::path::PathBuf::from("/tmp/cuenv-darwin-arm64.tar.gz.sha256"),
        }];

        let formula = backend.generate_formula(&ctx, &artifacts);
        assert!(formula.contains("class Cuenv < Formula"));
        assert!(formula.contains("version \"1.0.0\""));
        assert!(formula.contains("abcdef123456"));
        assert!(formula.contains("cuenv-darwin-arm64.tar.gz"));
    }

    #[test]
    fn test_generate_formula_default_base_url() {
        let config = HomebrewConfig::new("owner/tap", "myapp");
        let backend = HomebrewBackend::new(config);

        let ctx = BackendContext {
            name: "myapp".to_string(),
            version: "2.0.0".to_string(),
            download_base_url: None, // Uses default
            dry_run: DryRun::No,
        };

        let artifacts = vec![PackagedArtifact {
            target: Target::LinuxX64,
            archive_name: "myapp-linux-x64.tar.gz".to_string(),
            sha256: "789xyz".to_string(),
            archive_path: std::path::PathBuf::from("/tmp/myapp.tar.gz"),
            checksum_path: std::path::PathBuf::from("/tmp/myapp.tar.gz.sha256"),
        }];

        let formula = backend.generate_formula(&ctx, &artifacts);
        // Should use default base URL
        assert!(formula.contains("https://github.com/OWNER/REPO/releases/download"));
    }

    #[test]
    fn test_generate_formula_capitalizes_class_name() {
        let config = HomebrewConfig::new("owner/tap", "myapp");
        let backend = HomebrewBackend::new(config);

        let ctx = BackendContext {
            name: "myapp".to_string(),
            version: "1.0.0".to_string(),
            download_base_url: None,
            dry_run: DryRun::No,
        };

        let formula = backend.generate_formula(&ctx, &[]);
        assert!(formula.contains("class Myapp < Formula"));
    }

    #[test]
    fn test_generate_formula_with_multiple_artifacts() {
        let config = HomebrewConfig::new("owner/tap", "tool")
            .with_license("MIT")
            .with_homepage("https://tool.dev");
        let backend = HomebrewBackend::new(config);

        let ctx = BackendContext {
            name: "tool".to_string(),
            version: "3.0.0".to_string(),
            download_base_url: Some("https://releases.tool.dev".to_string()),
            dry_run: DryRun::No,
        };

        let artifacts = vec![
            PackagedArtifact {
                target: Target::DarwinArm64,
                archive_name: "tool-darwin-arm64.tar.gz".to_string(),
                sha256: "darwin_hash".to_string(),
                archive_path: std::path::PathBuf::from("/tmp/tool-darwin.tar.gz"),
                checksum_path: std::path::PathBuf::from("/tmp/tool-darwin.tar.gz.sha256"),
            },
            PackagedArtifact {
                target: Target::LinuxX64,
                archive_name: "tool-linux-x64.tar.gz".to_string(),
                sha256: "linux_hash".to_string(),
                archive_path: std::path::PathBuf::from("/tmp/tool-linux.tar.gz"),
                checksum_path: std::path::PathBuf::from("/tmp/tool-linux.tar.gz.sha256"),
            },
        ];

        let formula = backend.generate_formula(&ctx, &artifacts);
        assert!(formula.contains("darwin_hash"));
        assert!(formula.contains("linux_hash"));
        assert!(formula.contains("on_macos do"));
        assert!(formula.contains("on_linux do"));
    }

    #[test]
    fn test_config_get_token_from_direct() {
        let config = HomebrewConfig::new("owner/tap", "formula").with_token("direct-token");
        assert_eq!(config.get_token(), Some("direct-token".to_string()));
    }

    #[test]
    fn test_config_get_token_missing() {
        // Use a unique env var name that won't be set
        let config = HomebrewConfig::new("owner/tap", "formula")
            .with_token_env("CUENV_TEST_HOMEBREW_TOKEN_DEFINITELY_MISSING_12345");
        assert!(config.get_token().is_none());
    }
}
