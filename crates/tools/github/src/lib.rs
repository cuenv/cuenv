//! GitHub Releases tool provider for cuenv.
//!
//! Fetches development tools from GitHub Releases. Supports:
//! - Template variables in asset names: `{version}`, `{os}`, `{arch}`
//! - Automatic archive extraction (zip, tar.gz, tar.xz, pkg)
//! - Path-based binary extraction from archives

mod extract;

use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_core::http::ensure_rustls_crypto_provider;
use cuenv_core::tools::{
    Arch, FetchedTool, Os, Platform, ResolvedTool, ToolExtract, ToolOptions, ToolProvider,
    ToolResolveRequest, ToolSource,
};
use reqwest::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::io::AsyncReadExt;
use tracing::{debug, info};

/// Rate limit information from GitHub API response headers.
#[derive(Debug, Default)]
struct RateLimitInfo {
    /// Maximum requests allowed per hour.
    limit: Option<u32>,
    /// Remaining requests in the current window.
    remaining: Option<u32>,
    /// Unix timestamp when the rate limit resets.
    reset: Option<u64>,
}

impl RateLimitInfo {
    /// Extract rate limit info from response headers.
    fn from_headers(headers: &reqwest::header::HeaderMap) -> Self {
        Self {
            limit: headers
                .get("x-ratelimit-limit")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok()),
            remaining: headers
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok()),
            reset: headers
                .get("x-ratelimit-reset")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok()),
        }
    }

    /// Check if the rate limit has been exceeded.
    fn is_exceeded(&self) -> bool {
        self.remaining == Some(0)
    }

    /// Format the reset time as a human-readable duration.
    fn format_reset_duration(&self) -> Option<String> {
        let reset_ts = self.reset?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();

        if reset_ts <= now {
            return Some("now".to_string());
        }

        let seconds_remaining = reset_ts - now;
        let minutes = seconds_remaining / 60;
        let hours = minutes / 60;

        if hours > 0 {
            Some(format!("{} hour(s) {} minute(s)", hours, minutes % 60))
        } else if minutes > 0 {
            Some(format!("{} minute(s)", minutes))
        } else {
            Some(format!("{} second(s)", seconds_remaining))
        }
    }

    /// Format rate limit status (e.g., "0/60 requests remaining").
    fn format_status(&self) -> Option<String> {
        match (self.remaining, self.limit) {
            (Some(remaining), Some(limit)) => {
                Some(format!("{}/{} requests remaining", remaining, limit))
            }
            _ => None,
        }
    }
}

/// GitHub release metadata from the API.
#[derive(Debug, Deserialize)]
struct Release {
    #[allow(dead_code)] // Deserialized from GitHub API response
    tag_name: String,
    assets: Vec<Asset>,
}

/// GitHub release asset.
#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Tool provider for GitHub Releases.
///
/// Fetches binaries from GitHub Releases, supporting template expansion
/// for platform-specific asset names.
pub struct GitHubToolProvider {
    client: OnceLock<std::result::Result<Client, String>>,
}

impl Default for GitHubToolProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl GitHubToolProvider {
    fn build_client() -> std::result::Result<Client, String> {
        ensure_rustls_crypto_provider();

        let primary = catch_unwind(AssertUnwindSafe(|| {
            Client::builder().user_agent("cuenv").build()
        }));

        match primary {
            Ok(Ok(client)) => Ok(client),
            Ok(Err(primary_err)) => Client::builder()
                .user_agent("cuenv")
                .no_proxy()
                .build()
                .map_err(|fallback_err| {
                    format!(
                        "Failed to create GitHub HTTP client: primary={primary_err}; fallback={fallback_err}"
                    )
                }),
            Err(_) => Client::builder()
                .user_agent("cuenv")
                .no_proxy()
                .build()
                .map_err(|fallback_err| {
                    format!(
                        "Failed to create GitHub HTTP client after system proxy discovery panicked: fallback={fallback_err}"
                    )
                }),
        }
    }

    fn client(&self) -> Result<&Client> {
        match self.client.get_or_init(Self::build_client) {
            Ok(client) => Ok(client),
            Err(error) => Err(cuenv_core::Error::tool_resolution(error.clone())),
        }
    }

    /// Create a new GitHub tool provider.
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: OnceLock::new(),
        }
    }

    /// Get the cache directory for a tool.
    fn tool_cache_dir(&self, options: &ToolOptions, name: &str, version: &str) -> PathBuf {
        options.cache_dir().join("github").join(name).join(version)
    }

    /// Expand template variables in a string.
    fn expand_template(&self, template: &str, version: &str, platform: &Platform) -> String {
        let os_str = match platform.os {
            Os::Darwin => "darwin",
            Os::Linux => "linux",
        };
        let arch_str = match platform.arch {
            Arch::Arm64 => "aarch64",
            Arch::X86_64 => "x86_64",
        };

        template
            .replace("{version}", version)
            .replace("{os}", os_str)
            .replace("{arch}", arch_str)
    }

    /// Get the effective token: GITHUB_TOKEN > GH_TOKEN > runtime token
    fn get_effective_token(runtime_token: Option<&str>) -> Option<String> {
        std::env::var("GITHUB_TOKEN")
            .ok()
            .or_else(|| std::env::var("GH_TOKEN").ok())
            .or_else(|| runtime_token.map(String::from))
    }

    /// Fetch release information from GitHub API.
    async fn fetch_release(&self, repo: &str, tag: &str, token: Option<&str>) -> Result<Release> {
        let url = format!(
            "https://api.github.com/repos/{}/releases/tags/{}",
            repo, tag
        );
        debug!(%url, "Fetching GitHub release");

        let effective_token = Self::get_effective_token(token);
        let is_authenticated = effective_token.is_some();
        let client = self.client()?;

        let mut request = client.get(&url);
        if let Some(token) = effective_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request.send().await.map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to fetch release: {}", e))
        })?;

        let status = response.status();
        if !status.is_success() {
            let rate_limit = RateLimitInfo::from_headers(response.headers());

            return Err(Self::build_api_error(
                status,
                &rate_limit,
                is_authenticated,
                &format!("release {} {}", repo, tag),
            ));
        }

        response.json().await.map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to parse release: {}", e))
        })
    }

    /// Build an appropriate error for GitHub API failures.
    fn build_api_error(
        status: reqwest::StatusCode,
        rate_limit: &RateLimitInfo,
        is_authenticated: bool,
        resource: &str,
    ) -> cuenv_core::Error {
        // Handle rate limit exceeded (403 with remaining=0)
        if status == reqwest::StatusCode::FORBIDDEN && rate_limit.is_exceeded() {
            let mut message = "GitHub API rate limit exceeded".to_string();

            if let Some(status_str) = rate_limit.format_status() {
                message.push_str(&format!(" ({})", status_str));
            }
            if let Some(reset_str) = rate_limit.format_reset_duration() {
                message.push_str(&format!(". Resets in {}", reset_str));
            }

            let help = if is_authenticated {
                "Wait for the rate limit to reset, or use a different GitHub token"
            } else {
                "Set GITHUB_TOKEN environment variable with `public_repo` scope \
                 for 5000 requests/hour (unauthenticated: 60/hour)"
            };

            return cuenv_core::Error::tool_resolution_with_help(message, help);
        }

        // Handle 403 Forbidden (not rate limited)
        if status == reqwest::StatusCode::FORBIDDEN {
            let message = format!("Access denied to {} (HTTP 403 Forbidden)", resource);
            let help = if is_authenticated {
                "Check that your GITHUB_TOKEN has the required permissions. \
                 For private repositories, ensure the token has the `repo` scope"
            } else {
                "Set GITHUB_TOKEN environment variable with `public_repo` scope to access this resource"
            };
            return cuenv_core::Error::tool_resolution_with_help(message, help);
        }

        // Handle 404 Not Found
        if status == reqwest::StatusCode::NOT_FOUND {
            return cuenv_core::Error::tool_resolution(format!(
                "{} not found (HTTP 404)",
                resource
            ));
        }

        // Handle 401 Unauthorized
        if status == reqwest::StatusCode::UNAUTHORIZED {
            let help = "Your GITHUB_TOKEN may be invalid or expired. \
                       Generate a new token at https://github.com/settings/tokens";
            return cuenv_core::Error::tool_resolution_with_help(
                format!("Authentication failed for {} (HTTP 401)", resource),
                help,
            );
        }

        // Generic error for other status codes
        cuenv_core::Error::tool_resolution(format!("Failed to fetch {}: HTTP {}", resource, status))
    }

    /// Download an asset from GitHub.
    async fn download_asset(&self, url: &str, token: Option<&str>) -> Result<Vec<u8>> {
        debug!(%url, "Downloading GitHub asset");

        let effective_token = Self::get_effective_token(token);
        let is_authenticated = effective_token.is_some();
        let client = self.client()?;

        let mut request = client.get(url);
        if let Some(token) = effective_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request.send().await.map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to download asset: {}", e))
        })?;

        let status = response.status();
        if !status.is_success() {
            let rate_limit = RateLimitInfo::from_headers(response.headers());
            return Err(Self::build_api_error(
                status,
                &rate_limit,
                is_authenticated,
                "asset download",
            ));
        }

        response
            .bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| cuenv_core::Error::tool_resolution(format!("Failed to read asset: {}", e)))
    }

    /// Determine whether a path looks like a dynamic library.
    fn path_looks_like_library(path: &str) -> bool {
        let path_lower = path.to_ascii_lowercase();
        path_lower.ends_with(".dylib")
            || path_lower.ends_with(".so")
            || path_lower.contains(".so.")
            || path_lower.ends_with(".dll")
    }

    /// Determine whether a filesystem path looks like a dynamic library.
    fn file_looks_like_library(path: &Path) -> bool {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        name.ends_with(".dylib")
            || name.ends_with(".so")
            || name.contains(".so.")
            || name.ends_with(".dll")
    }

    fn expand_extract_templates(
        &self,
        extract: &[ToolExtract],
        version: &str,
        platform: &Platform,
    ) -> Vec<ToolExtract> {
        extract
            .iter()
            .map(|item| match item {
                ToolExtract::Bin { path, as_name } => ToolExtract::Bin {
                    path: self.expand_template(path, version, platform),
                    as_name: as_name.clone(),
                },
                ToolExtract::Lib { path, env } => ToolExtract::Lib {
                    path: self.expand_template(path, version, platform),
                    env: env.clone(),
                },
                ToolExtract::Include { path } => ToolExtract::Include {
                    path: self.expand_template(path, version, platform),
                },
                ToolExtract::PkgConfig { path } => ToolExtract::PkgConfig {
                    path: self.expand_template(path, version, platform),
                },
                ToolExtract::File { path, env } => ToolExtract::File {
                    path: self.expand_template(path, version, platform),
                    env: env.clone(),
                },
            })
            .collect()
    }

    fn cache_targets_from_source(
        &self,
        resolved: &ResolvedTool,
        options: &ToolOptions,
    ) -> Vec<PathBuf> {
        let cache_dir = self.tool_cache_dir(options, &resolved.name, &resolved.version);
        let extract = match &resolved.source {
            ToolSource::GitHub { extract, .. } => extract,
            _ => return vec![cache_dir.join("bin").join(&resolved.name)],
        };

        if extract.is_empty() {
            return vec![cache_dir.join("bin").join(&resolved.name)];
        }

        extract
            .iter()
            .map(|item| self.cache_target_for_extract(&cache_dir, &resolved.name, item))
            .collect()
    }

    fn cache_target_for_extract(
        &self,
        cache_dir: &Path,
        tool_name: &str,
        item: &ToolExtract,
    ) -> PathBuf {
        match item {
            ToolExtract::Bin { path, as_name } => {
                let name = as_name.as_deref().unwrap_or_else(|| {
                    Path::new(path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(tool_name)
                });
                cache_dir.join("bin").join(name)
            }
            ToolExtract::Lib { path, .. } => {
                let name = Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(tool_name);
                cache_dir.join("lib").join(name)
            }
            ToolExtract::Include { path } => {
                let name = Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(tool_name);
                cache_dir.join("include").join(name)
            }
            ToolExtract::PkgConfig { path } => {
                let name = Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(tool_name);
                cache_dir.join("lib").join("pkgconfig").join(name)
            }
            ToolExtract::File { path, .. } => {
                let name = Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(tool_name);
                cache_dir.join("files").join(name)
            }
        }
    }

    fn is_executable_extract(item: &ToolExtract) -> bool {
        matches!(item, ToolExtract::Bin { .. })
    }

    fn extract_source_path(item: &ToolExtract) -> &str {
        match item {
            ToolExtract::Bin { path, .. }
            | ToolExtract::Lib { path, .. }
            | ToolExtract::Include { path }
            | ToolExtract::PkgConfig { path }
            | ToolExtract::File { path, .. } => path,
        }
    }
}

#[async_trait]
impl ToolProvider for GitHubToolProvider {
    fn name(&self) -> &'static str {
        "github"
    }

    fn description(&self) -> &'static str {
        "Fetch tools from GitHub Releases"
    }

    fn can_handle(&self, source: &ToolSource) -> bool {
        matches!(source, ToolSource::GitHub { .. })
    }

    async fn resolve(&self, request: &ToolResolveRequest<'_>) -> Result<ResolvedTool> {
        let tool_name = request.tool_name;
        let version = request.version;
        let platform = request.platform;
        let config = request.config;
        let token = request.token;

        let repo = config
            .get("repo")
            .and_then(|v| v.as_str())
            .ok_or_else(|| cuenv_core::Error::tool_resolution("Missing 'repo' in config"))?;

        let asset_template = config
            .get("asset")
            .and_then(|v| v.as_str())
            .ok_or_else(|| cuenv_core::Error::tool_resolution("Missing 'asset' in config"))?;

        let tag_template = config
            .get("tag")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                let prefix = config
                    .get("tagPrefix")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                format!("{prefix}{{version}}")
            });

        let extract: Vec<ToolExtract> = config
            .get("extract")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| {
                cuenv_core::Error::tool_resolution(format!(
                    "Invalid 'extract' in GitHub source config: {}",
                    e
                ))
            })?
            .unwrap_or_default();
        let path = config
            .get("path")
            .and_then(|v| v.as_str())
            .map(String::from);

        info!(%tool_name, %repo, %version, %platform, "Resolving GitHub release");

        // Expand templates
        let tag = self.expand_template(&tag_template, version, platform);
        let asset = self.expand_template(asset_template, version, platform);
        let mut expanded_extract = self.expand_extract_templates(&extract, version, platform);
        if expanded_extract.is_empty()
            && let Some(path) = path.as_deref()
        {
            let expanded_path = self.expand_template(path, version, platform);
            if Self::path_looks_like_library(&expanded_path) {
                expanded_extract.push(ToolExtract::Lib {
                    path: expanded_path,
                    env: None,
                });
            } else {
                expanded_extract.push(ToolExtract::Bin {
                    path: expanded_path,
                    as_name: None,
                });
            }
        }

        // Fetch release to verify it exists (uses runtime token if provided)
        let release = self.fetch_release(repo, &tag, token).await?;

        // Find the asset
        let found_asset = release.assets.iter().find(|a| a.name == asset);
        if found_asset.is_none() {
            let available: Vec<_> = release.assets.iter().map(|a| &a.name).collect();
            return Err(cuenv_core::Error::tool_resolution(format!(
                "Asset '{}' not found in release. Available: {:?}",
                asset, available
            )));
        }

        debug!(%tag, %asset, "Resolved GitHub release");

        Ok(ResolvedTool {
            name: tool_name.to_string(),
            version: version.to_string(),
            platform: platform.clone(),
            source: ToolSource::GitHub {
                repo: repo.to_string(),
                tag,
                asset,
                extract: expanded_extract,
            },
        })
    }

    async fn fetch(&self, resolved: &ResolvedTool, options: &ToolOptions) -> Result<FetchedTool> {
        let ToolSource::GitHub {
            repo,
            tag,
            asset,
            extract,
        } = &resolved.source
        else {
            return Err(cuenv_core::Error::tool_resolution(
                "GitHubToolProvider received non-GitHub source".to_string(),
            ));
        };

        info!(
            tool = %resolved.name,
            %repo,
            %tag,
            %asset,
            "Fetching GitHub release"
        );

        // Check cache
        let cache_dir = self.tool_cache_dir(options, &resolved.name, &resolved.version);
        let cached_targets = self.cache_targets_from_source(resolved, options);
        if !options.force_refetch && cached_targets.iter().all(|p| p.exists()) {
            let cached_path = cached_targets
                .first()
                .cloned()
                .unwrap_or_else(|| cache_dir.join("bin").join(&resolved.name));
            debug!(?cached_path, "Tool already cached");
            let sha256 = compute_file_sha256(&cached_path).await?;
            return Ok(FetchedTool {
                name: resolved.name.clone(),
                binary_path: cached_path,
                sha256,
            });
        }

        // Fetch release and download asset (no runtime token for fetch - uses env vars)
        let release = self.fetch_release(repo, tag, None).await?;
        let found_asset = release
            .assets
            .iter()
            .find(|a| &a.name == asset)
            .ok_or_else(|| {
                cuenv_core::Error::tool_resolution(format!("Asset '{}' not found", asset))
            })?;

        let data = self
            .download_asset(&found_asset.browser_download_url, None)
            .await?;

        if extract.is_empty() {
            // Legacy behavior: single binary inferred from archive.
            let extracted = self.extract_binary(&data, asset, None, &cache_dir)?;
            let final_path = if Self::file_looks_like_library(&extracted) {
                let file_name = extracted
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&resolved.name);
                cache_dir.join("lib").join(file_name)
            } else {
                cache_dir.join("bin").join(&resolved.name)
            };
            if let Some(parent) = final_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if extracted != final_path {
                if final_path.exists() {
                    std::fs::remove_file(&final_path)?;
                }
                std::fs::rename(&extracted, &final_path)?;
            }
            if !Self::file_looks_like_library(&final_path) {
                Self::ensure_executable(&final_path)?;
            }

            let sha256 = compute_file_sha256(&final_path).await?;
            info!(
                tool = %resolved.name,
                binary = ?final_path,
                %sha256,
                "Fetched GitHub release"
            );
            return Ok(FetchedTool {
                name: resolved.name.clone(),
                binary_path: final_path,
                sha256,
            });
        }

        // Typed extract mode: fetch each declared artifact by path.
        let extract_dir = cache_dir.join(".extract");
        if extract_dir.exists() {
            std::fs::remove_dir_all(&extract_dir)?;
        }
        std::fs::create_dir_all(&extract_dir)?;

        let mut produced_paths: Vec<PathBuf> = Vec::with_capacity(extract.len());
        for item in extract {
            let source_path = Self::extract_source_path(item);
            let extracted_path =
                self.extract_binary(&data, asset, Some(source_path), &extract_dir)?;
            let final_path = self.cache_target_for_extract(&cache_dir, &resolved.name, item);
            if let Some(parent) = final_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if final_path.exists() {
                std::fs::remove_file(&final_path)?;
            }
            std::fs::rename(&extracted_path, &final_path)?;
            if Self::is_executable_extract(item) {
                Self::ensure_executable(&final_path)?;
            }
            produced_paths.push(final_path);
        }
        if extract_dir.exists() {
            let _ = std::fs::remove_dir_all(&extract_dir);
        }

        let primary_path = produced_paths
            .first()
            .cloned()
            .unwrap_or_else(|| cache_dir.join("bin").join(&resolved.name));
        let sha256 = compute_file_sha256(&primary_path).await?;
        info!(
            tool = %resolved.name,
            binary = ?primary_path,
            %sha256,
            "Fetched GitHub release"
        );

        Ok(FetchedTool {
            name: resolved.name.clone(),
            binary_path: primary_path,
            sha256,
        })
    }

    fn is_cached(&self, resolved: &ResolvedTool, options: &ToolOptions) -> bool {
        self.cache_targets_from_source(resolved, options)
            .into_iter()
            .all(|path| path.exists())
    }
}

/// Compute SHA256 hash of a file.
async fn compute_file_sha256(path: &std::path::Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 8192];

    loop {
        let n = file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests;
