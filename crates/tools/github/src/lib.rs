//! GitHub Releases tool provider for cuenv.
//!
//! Fetches development tools from GitHub Releases. Supports:
//! - Template variables in asset names: `{version}`, `{os}`, `{arch}`
//! - Automatic archive extraction (zip, tar.gz)
//! - Path-based binary extraction from archives

use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_core::tools::{
    Arch, FetchedTool, Os, Platform, ResolvedTool, ToolOptions, ToolProvider, ToolSource,
};
use flate2::read::GzDecoder;
use reqwest::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read};
use std::path::PathBuf;
use tar::Archive;
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
    #[allow(dead_code)]
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
    client: Client,
}

impl Default for GitHubToolProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl GitHubToolProvider {
    /// Create a new GitHub tool provider.
    ///
    /// # Panics
    ///
    /// This function uses `expect` internally because `reqwest::Client::builder().build()`
    /// only fails with invalid TLS configuration, which cannot happen with default settings.
    /// The panic is acceptable here as it indicates a fundamental environment issue.
    #[must_use]
    pub fn new() -> Self {
        // SAFETY: Client::builder().build() only fails if:
        // 1. TLS backend fails to initialize (system-level issue)
        // 2. Invalid proxy configuration (we don't set any)
        // With default settings and user_agent only, this cannot fail.
        Self {
            client: Client::builder()
                .user_agent("cuenv")
                .build()
                .expect("Failed to create HTTP client - TLS backend initialization failed"),
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

        let mut request = self.client.get(&url);
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

        let mut request = self.client.get(url);
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

    /// Extract a binary from an archive.
    fn extract_binary(
        &self,
        data: &[u8],
        asset_name: &str,
        binary_path: Option<&str>,
        dest: &std::path::Path,
    ) -> Result<PathBuf> {
        // Determine archive type
        let is_zip = asset_name.ends_with(".zip");
        let is_tar_gz = asset_name.ends_with(".tar.gz") || asset_name.ends_with(".tgz");

        if is_zip {
            self.extract_from_zip(data, binary_path, dest)
        } else if is_tar_gz {
            self.extract_from_tar_gz(data, binary_path, dest)
        } else {
            // Assume it's a raw binary
            std::fs::create_dir_all(dest)?;
            let binary_name = std::path::Path::new(asset_name)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(asset_name);
            let binary_dest = dest.join(binary_name);
            std::fs::write(&binary_dest, data)?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&binary_dest)?.permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&binary_dest, perms)?;
            }

            Ok(binary_dest)
        }
    }

    /// Extract from a zip archive.
    ///
    /// Uses a temporary directory for atomic extraction - if extraction fails
    /// partway through, no partial files are left in the destination.
    fn extract_from_zip(
        &self,
        data: &[u8],
        binary_path: Option<&str>,
        dest: &std::path::Path,
    ) -> Result<PathBuf> {
        let cursor = Cursor::new(data);
        let mut archive = zip::ZipArchive::new(cursor).map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to open zip: {}", e))
        })?;

        // If a specific path is requested, extract just that file (no temp dir needed)
        if let Some(path) = binary_path {
            for i in 0..archive.len() {
                let mut file = archive.by_index(i).map_err(|e| {
                    cuenv_core::Error::tool_resolution(format!("Failed to read zip entry: {}", e))
                })?;

                let name = file.name().to_string();
                if name.ends_with(path) || name == path {
                    std::fs::create_dir_all(dest)?;
                    let file_name = std::path::Path::new(&name)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(path);
                    let dest_path = dest.join(file_name);

                    let mut content = Vec::new();
                    file.read_to_end(&mut content)?;
                    std::fs::write(&dest_path, &content)?;

                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let mut perms = std::fs::metadata(&dest_path)?.permissions();
                        perms.set_mode(0o755);
                        std::fs::set_permissions(&dest_path, perms)?;
                    }

                    return Ok(dest_path);
                }
            }

            return Err(cuenv_core::Error::tool_resolution(format!(
                "Binary '{}' not found in archive",
                path
            )));
        }

        // Extract all files to a temp directory first for atomic operation
        let temp_dir = dest.with_file_name(format!(
            ".{}.tmp",
            dest.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("extract")
        ));

        // Clean up any previous failed extraction
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir)?;
        }
        std::fs::create_dir_all(&temp_dir)?;

        // Extract to temp directory
        let extract_result = (|| -> Result<()> {
            for i in 0..archive.len() {
                let mut file = archive.by_index(i).map_err(|e| {
                    cuenv_core::Error::tool_resolution(format!("Failed to read zip entry: {}", e))
                })?;

                let outpath = match file.enclosed_name() {
                    Some(path) => temp_dir.join(path),
                    None => continue,
                };

                if file.is_dir() {
                    std::fs::create_dir_all(&outpath)?;
                } else {
                    if let Some(p) = outpath.parent() {
                        std::fs::create_dir_all(p)?;
                    }
                    let mut content = Vec::new();
                    file.read_to_end(&mut content)?;
                    std::fs::write(&outpath, &content)?;

                    #[cfg(unix)]
                    if let Some(mode) = file.unix_mode() {
                        use std::os::unix::fs::PermissionsExt;
                        let mut perms = std::fs::metadata(&outpath)?.permissions();
                        perms.set_mode(mode);
                        std::fs::set_permissions(&outpath, perms)?;
                    }
                }
            }
            Ok(())
        })();

        // On failure, clean up temp directory
        if let Err(e) = extract_result {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err(e);
        }

        // Atomic move: remove destination if exists, then rename temp to dest
        if dest.exists() {
            std::fs::remove_dir_all(dest)?;
        }
        std::fs::rename(&temp_dir, dest)?;

        // Find the main binary (first executable in bin/ or root)
        self.find_main_binary(dest)
    }

    /// Extract from a tar.gz archive.
    fn extract_from_tar_gz(
        &self,
        data: &[u8],
        binary_path: Option<&str>,
        dest: &std::path::Path,
    ) -> Result<PathBuf> {
        let cursor = Cursor::new(data);
        let decoder = GzDecoder::new(cursor);
        let mut archive = Archive::new(decoder);

        std::fs::create_dir_all(dest)?;

        if let Some(path) = binary_path {
            // Look for specific file
            for entry in archive.entries().map_err(|e| {
                cuenv_core::Error::tool_resolution(format!("Failed to read tar: {}", e))
            })? {
                let mut entry = entry.map_err(|e| {
                    cuenv_core::Error::tool_resolution(format!("Failed to read tar entry: {}", e))
                })?;

                let entry_path = entry.path().map_err(|e| {
                    cuenv_core::Error::tool_resolution(format!("Invalid path in tar: {}", e))
                })?;

                let path_str = entry_path.to_string_lossy();
                if path_str.ends_with(path) || path_str.as_ref() == path {
                    let file_name = std::path::Path::new(path)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(path);
                    let dest_path = dest.join(file_name);

                    let mut content = Vec::new();
                    entry.read_to_end(&mut content)?;
                    std::fs::write(&dest_path, &content)?;

                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let mut perms = std::fs::metadata(&dest_path)?.permissions();
                        perms.set_mode(0o755);
                        std::fs::set_permissions(&dest_path, perms)?;
                    }

                    return Ok(dest_path);
                }
            }

            return Err(cuenv_core::Error::tool_resolution(format!(
                "Binary '{}' not found in archive",
                path
            )));
        }

        // Extract all files
        archive.unpack(dest).map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to extract tar: {}", e))
        })?;

        // Find the main binary
        self.find_main_binary(dest)
    }

    /// Find the main binary in an extracted directory.
    fn find_main_binary(&self, dir: &std::path::Path) -> Result<PathBuf> {
        // First, look for binaries in bin/
        let bin_dir = dir.join("bin");
        if bin_dir.exists() {
            for entry in std::fs::read_dir(&bin_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_file() {
                    return Ok(path);
                }
            }
        }

        // Then look for executables in root
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = std::fs::metadata(&path) {
                        if meta.permissions().mode() & 0o111 != 0 {
                            return Ok(path);
                        }
                    }
                }
                #[cfg(not(unix))]
                {
                    // On non-Unix, just return the first file
                    return Ok(path);
                }
            }
        }

        Err(cuenv_core::Error::tool_resolution(
            "No binary found in extracted archive".to_string(),
        ))
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

    async fn resolve(
        &self,
        tool_name: &str,
        version: &str,
        platform: &Platform,
        config: &serde_json::Value,
    ) -> Result<ResolvedTool> {
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

        let path = config.get("path").and_then(|v| v.as_str());

        info!(%tool_name, %repo, %version, %platform, "Resolving GitHub release");

        // Expand templates
        let tag = self.expand_template(&tag_template, version, platform);
        let asset = self.expand_template(asset_template, version, platform);
        let expanded_path = path.map(|p| self.expand_template(p, version, platform));

        // Fetch release to verify it exists (no runtime token for backward compat)
        let release = self.fetch_release(repo, &tag, None).await?;

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
                path: expanded_path,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn resolve_with_token(
        &self,
        tool_name: &str,
        version: &str,
        platform: &Platform,
        config: &serde_json::Value,
        token: Option<&str>,
    ) -> Result<ResolvedTool> {
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

        let path = config.get("path").and_then(|v| v.as_str());

        info!(%tool_name, %repo, %version, %platform, "Resolving GitHub release with token");

        // Expand templates
        let tag = self.expand_template(&tag_template, version, platform);
        let asset = self.expand_template(asset_template, version, platform);
        let expanded_path = path.map(|p| self.expand_template(p, version, platform));

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
                path: expanded_path,
            },
        })
    }

    async fn fetch(&self, resolved: &ResolvedTool, options: &ToolOptions) -> Result<FetchedTool> {
        let ToolSource::GitHub {
            repo,
            tag,
            asset,
            path,
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

        // Check cache - binaries go in bin/ subdirectory for consistency with other providers
        let cache_dir = self.tool_cache_dir(options, &resolved.name, &resolved.version);
        let bin_dir = cache_dir.join("bin");
        let binary_path = bin_dir.join(&resolved.name);

        if binary_path.exists() && !options.force_refetch {
            debug!(?binary_path, "Tool already cached");
            let sha256 = compute_file_sha256(&binary_path).await?;
            return Ok(FetchedTool {
                name: resolved.name.clone(),
                binary_path,
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

        // Ensure bin directory exists
        std::fs::create_dir_all(&bin_dir)?;

        // Extract binary to a temp location first, then move to bin/
        let extracted = self.extract_binary(&data, asset, path.as_deref(), &cache_dir)?;

        // Move to bin/<tool_name> for consistency with other providers
        let final_path = bin_dir.join(&resolved.name);
        if extracted != final_path {
            // If extracted to a different location, move it
            if final_path.exists() {
                std::fs::remove_file(&final_path)?;
            }
            std::fs::rename(&extracted, &final_path)?;
        }

        let sha256 = compute_file_sha256(&final_path).await?;

        info!(
            tool = %resolved.name,
            binary = ?final_path,
            %sha256,
            "Fetched GitHub release"
        );

        Ok(FetchedTool {
            name: resolved.name.clone(),
            binary_path: final_path,
            sha256,
        })
    }

    fn is_cached(&self, resolved: &ResolvedTool, options: &ToolOptions) -> bool {
        let cache_dir = self.tool_cache_dir(options, &resolved.name, &resolved.version);
        let binary_path = cache_dir.join("bin").join(&resolved.name);
        binary_path.exists()
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
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ==========================================================================
    // GitHubToolProvider construction and ToolProvider trait tests
    // ==========================================================================

    #[test]
    fn test_provider_name() {
        let provider = GitHubToolProvider::new();
        assert_eq!(provider.name(), "github");
    }

    #[test]
    fn test_provider_description() {
        let provider = GitHubToolProvider::new();
        assert_eq!(provider.description(), "Fetch tools from GitHub Releases");
    }

    #[test]
    fn test_provider_default() {
        let provider = GitHubToolProvider::default();
        assert_eq!(provider.name(), "github");
    }

    #[test]
    fn test_can_handle() {
        let provider = GitHubToolProvider::new();

        let github_source = ToolSource::GitHub {
            repo: "org/repo".into(),
            tag: "v1".into(),
            asset: "file.zip".into(),
            path: None,
        };
        assert!(provider.can_handle(&github_source));

        let nix_source = ToolSource::Nix {
            flake: "nixpkgs".into(),
            package: "jq".into(),
            output: None,
        };
        assert!(!provider.can_handle(&nix_source));
    }

    #[test]
    fn test_can_handle_github_with_path() {
        let provider = GitHubToolProvider::new();

        let source = ToolSource::GitHub {
            repo: "owner/repo".into(),
            tag: "v1.0.0".into(),
            asset: "archive.tar.gz".into(),
            path: Some("bin/tool".into()),
        };
        assert!(provider.can_handle(&source));
    }

    // ==========================================================================
    // expand_template tests
    // ==========================================================================

    #[test]
    fn test_expand_template() {
        let provider = GitHubToolProvider::new();
        let platform = Platform::new(Os::Darwin, Arch::Arm64);

        assert_eq!(
            provider.expand_template("bun-{os}-{arch}.zip", "1.0.0", &platform),
            "bun-darwin-aarch64.zip"
        );

        assert_eq!(
            provider.expand_template("v{version}", "1.0.0", &platform),
            "v1.0.0"
        );
    }

    #[test]
    fn test_expand_template_linux_x86_64() {
        let provider = GitHubToolProvider::new();
        let platform = Platform::new(Os::Linux, Arch::X86_64);

        assert_eq!(
            provider.expand_template("{os}-{arch}", "1.0.0", &platform),
            "linux-x86_64"
        );
    }

    #[test]
    fn test_expand_template_all_placeholders() {
        let provider = GitHubToolProvider::new();
        let platform = Platform::new(Os::Darwin, Arch::X86_64);

        assert_eq!(
            provider.expand_template("tool-{version}-{os}-{arch}.zip", "2.5.1", &platform),
            "tool-2.5.1-darwin-x86_64.zip"
        );
    }

    #[test]
    fn test_expand_template_no_placeholders() {
        let provider = GitHubToolProvider::new();
        let platform = Platform::new(Os::Linux, Arch::Arm64);

        assert_eq!(
            provider.expand_template("static-name.tar.gz", "1.0.0", &platform),
            "static-name.tar.gz"
        );
    }

    // ==========================================================================
    // tool_cache_dir tests
    // ==========================================================================

    #[test]
    fn test_tool_cache_dir() {
        let provider = GitHubToolProvider::new();
        let temp_dir = TempDir::new().unwrap();
        let options = ToolOptions::new().with_cache_dir(temp_dir.path().to_path_buf());

        let cache_dir = provider.tool_cache_dir(&options, "mytool", "1.2.3");

        assert!(cache_dir.ends_with("github/mytool/1.2.3"));
        assert!(cache_dir.starts_with(temp_dir.path()));
    }

    #[test]
    fn test_tool_cache_dir_different_versions() {
        let provider = GitHubToolProvider::new();
        let temp_dir = TempDir::new().unwrap();
        let options = ToolOptions::new().with_cache_dir(temp_dir.path().to_path_buf());

        let cache_v1 = provider.tool_cache_dir(&options, "tool", "1.0.0");
        let cache_v2 = provider.tool_cache_dir(&options, "tool", "2.0.0");

        assert_ne!(cache_v1, cache_v2);
        assert!(cache_v1.ends_with("1.0.0"));
        assert!(cache_v2.ends_with("2.0.0"));
    }

    // ==========================================================================
    // get_effective_token tests
    // ==========================================================================

    #[test]
    fn test_get_effective_token_runtime_only() {
        // Clear env vars first
        // SAFETY: Test runs in isolation
        unsafe {
            std::env::remove_var("GITHUB_TOKEN");
            std::env::remove_var("GH_TOKEN");
        }

        let token = GitHubToolProvider::get_effective_token(Some("runtime-token"));
        assert_eq!(token, Some("runtime-token".to_string()));
    }

    #[test]
    fn test_get_effective_token_none() {
        // SAFETY: Test runs in isolation
        unsafe {
            std::env::remove_var("GITHUB_TOKEN");
            std::env::remove_var("GH_TOKEN");
        }

        let token = GitHubToolProvider::get_effective_token(None);
        assert!(token.is_none());
    }

    #[test]
    fn test_get_effective_token_github_token_priority() {
        // SAFETY: Test runs in isolation
        unsafe {
            std::env::set_var("GITHUB_TOKEN", "github-token");
            std::env::set_var("GH_TOKEN", "gh-token");
        }

        let token = GitHubToolProvider::get_effective_token(Some("runtime-token"));
        assert_eq!(token, Some("github-token".to_string()));

        // SAFETY: Cleanup
        unsafe {
            std::env::remove_var("GITHUB_TOKEN");
            std::env::remove_var("GH_TOKEN");
        }
    }

    #[test]
    fn test_get_effective_token_gh_token_fallback() {
        // SAFETY: Test runs in isolation
        unsafe {
            std::env::remove_var("GITHUB_TOKEN");
            std::env::set_var("GH_TOKEN", "gh-token");
        }

        let token = GitHubToolProvider::get_effective_token(Some("runtime-token"));
        assert_eq!(token, Some("gh-token".to_string()));

        // SAFETY: Cleanup
        unsafe {
            std::env::remove_var("GH_TOKEN");
        }
    }

    // ==========================================================================
    // RateLimitInfo tests
    // ==========================================================================

    #[test]
    fn test_rate_limit_info_from_headers() {
        use reqwest::header::{HeaderMap, HeaderValue};

        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-limit", HeaderValue::from_static("60"));
        headers.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        headers.insert("x-ratelimit-reset", HeaderValue::from_static("1735689600"));

        let info = RateLimitInfo::from_headers(&headers);
        assert_eq!(info.limit, Some(60));
        assert_eq!(info.remaining, Some(0));
        assert_eq!(info.reset, Some(1_735_689_600));
        assert!(info.is_exceeded());
    }

    #[test]
    fn test_rate_limit_info_not_exceeded() {
        use reqwest::header::{HeaderMap, HeaderValue};

        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-limit", HeaderValue::from_static("5000"));
        headers.insert("x-ratelimit-remaining", HeaderValue::from_static("4999"));

        let info = RateLimitInfo::from_headers(&headers);
        assert!(!info.is_exceeded());
    }

    #[test]
    fn test_rate_limit_info_format_status() {
        let info = RateLimitInfo {
            limit: Some(60),
            remaining: Some(0),
            reset: None,
        };
        assert_eq!(
            info.format_status(),
            Some("0/60 requests remaining".to_string())
        );

        let info_partial = RateLimitInfo {
            limit: Some(60),
            remaining: None,
            reset: None,
        };
        assert_eq!(info_partial.format_status(), None);
    }

    #[test]
    fn test_rate_limit_info_empty_headers() {
        let headers = reqwest::header::HeaderMap::new();
        let info = RateLimitInfo::from_headers(&headers);

        assert_eq!(info.limit, None);
        assert_eq!(info.remaining, None);
        assert_eq!(info.reset, None);
        assert!(!info.is_exceeded());
    }

    #[test]
    fn test_rate_limit_info_default() {
        let info = RateLimitInfo::default();
        assert_eq!(info.limit, None);
        assert_eq!(info.remaining, None);
        assert_eq!(info.reset, None);
        assert!(!info.is_exceeded());
    }

    #[test]
    fn test_rate_limit_info_format_status_missing_remaining() {
        let info = RateLimitInfo {
            limit: Some(60),
            remaining: None,
            reset: None,
        };
        assert!(info.format_status().is_none());
    }

    #[test]
    fn test_rate_limit_info_format_status_missing_limit() {
        let info = RateLimitInfo {
            limit: None,
            remaining: Some(50),
            reset: None,
        };
        assert!(info.format_status().is_none());
    }

    #[test]
    fn test_rate_limit_info_format_reset_duration_none() {
        let info = RateLimitInfo {
            limit: None,
            remaining: None,
            reset: None,
        };
        assert!(info.format_reset_duration().is_none());
    }

    #[test]
    fn test_rate_limit_info_format_reset_duration_past() {
        // Use a timestamp in the past
        let info = RateLimitInfo {
            limit: None,
            remaining: None,
            reset: Some(0), // epoch
        };
        // Should return "now" for past timestamps
        assert_eq!(info.format_reset_duration(), Some("now".to_string()));
    }

    #[test]
    fn test_rate_limit_info_invalid_header_values() {
        use reqwest::header::{HeaderMap, HeaderValue};

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-ratelimit-limit",
            HeaderValue::from_static("not-a-number"),
        );
        headers.insert("x-ratelimit-remaining", HeaderValue::from_static("invalid"));

        let info = RateLimitInfo::from_headers(&headers);
        assert_eq!(info.limit, None);
        assert_eq!(info.remaining, None);
    }

    // ==========================================================================
    // build_api_error tests
    // ==========================================================================

    #[test]
    fn test_build_api_error_rate_limit_exceeded_unauthenticated() {
        let rate_limit = RateLimitInfo {
            limit: Some(60),
            remaining: Some(0),
            reset: Some(1_735_689_600),
        };

        let error = GitHubToolProvider::build_api_error(
            reqwest::StatusCode::FORBIDDEN,
            &rate_limit,
            false,
            "release owner/repo v1.0.0",
        );

        let msg = error.to_string();
        assert!(msg.contains("rate limit exceeded"));
    }

    #[test]
    fn test_build_api_error_rate_limit_exceeded_authenticated() {
        let rate_limit = RateLimitInfo {
            limit: Some(5000),
            remaining: Some(0),
            reset: None,
        };

        let error = GitHubToolProvider::build_api_error(
            reqwest::StatusCode::FORBIDDEN,
            &rate_limit,
            true,
            "release owner/repo v1.0.0",
        );

        let msg = error.to_string();
        assert!(msg.contains("rate limit exceeded"));
    }

    #[test]
    fn test_build_api_error_forbidden_not_rate_limit() {
        let rate_limit = RateLimitInfo {
            limit: Some(60),
            remaining: Some(30),
            reset: None,
        };

        let error = GitHubToolProvider::build_api_error(
            reqwest::StatusCode::FORBIDDEN,
            &rate_limit,
            false,
            "release owner/repo v1.0.0",
        );

        let msg = error.to_string();
        assert!(msg.contains("Access denied"));
    }

    #[test]
    fn test_build_api_error_not_found() {
        let rate_limit = RateLimitInfo::default();

        let error = GitHubToolProvider::build_api_error(
            reqwest::StatusCode::NOT_FOUND,
            &rate_limit,
            false,
            "release owner/repo v999.0.0",
        );

        let msg = error.to_string();
        assert!(msg.contains("not found"));
        assert!(msg.contains("404"));
    }

    #[test]
    fn test_build_api_error_unauthorized() {
        let rate_limit = RateLimitInfo::default();

        let error = GitHubToolProvider::build_api_error(
            reqwest::StatusCode::UNAUTHORIZED,
            &rate_limit,
            true,
            "release owner/repo v1.0.0",
        );

        let msg = error.to_string();
        assert!(msg.contains("Authentication failed"));
        assert!(msg.contains("401"));
    }

    #[test]
    fn test_build_api_error_server_error() {
        let rate_limit = RateLimitInfo::default();

        let error = GitHubToolProvider::build_api_error(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            &rate_limit,
            false,
            "asset download",
        );

        let msg = error.to_string();
        assert!(msg.contains("HTTP 500"));
    }

    // ==========================================================================
    // is_cached tests
    // ==========================================================================

    #[test]
    fn test_is_cached_not_cached() {
        let provider = GitHubToolProvider::new();
        let temp_dir = TempDir::new().unwrap();
        let options = ToolOptions::new().with_cache_dir(temp_dir.path().to_path_buf());

        let resolved = ResolvedTool {
            name: "mytool".to_string(),
            version: "1.0.0".to_string(),
            platform: Platform::new(Os::Darwin, Arch::Arm64),
            source: ToolSource::GitHub {
                repo: "owner/repo".to_string(),
                tag: "v1.0.0".to_string(),
                asset: "mytool.tar.gz".to_string(),
                path: None,
            },
        };

        assert!(!provider.is_cached(&resolved, &options));
    }

    #[test]
    fn test_is_cached_cached() {
        let provider = GitHubToolProvider::new();
        let temp_dir = TempDir::new().unwrap();
        let options = ToolOptions::new().with_cache_dir(temp_dir.path().to_path_buf());

        let resolved = ResolvedTool {
            name: "mytool".to_string(),
            version: "1.0.0".to_string(),
            platform: Platform::new(Os::Darwin, Arch::Arm64),
            source: ToolSource::GitHub {
                repo: "owner/repo".to_string(),
                tag: "v1.0.0".to_string(),
                asset: "mytool.tar.gz".to_string(),
                path: None,
            },
        };

        // Create the cached file
        let cache_dir = provider.tool_cache_dir(&options, "mytool", "1.0.0");
        let bin_dir = cache_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(bin_dir.join("mytool"), b"binary").unwrap();

        assert!(provider.is_cached(&resolved, &options));
    }

    // ==========================================================================
    // Release and Asset struct tests
    // ==========================================================================

    #[test]
    fn test_release_deserialization() {
        let json = r#"{
            "tag_name": "v1.0.0",
            "assets": [
                {"name": "tool-linux.tar.gz", "browser_download_url": "https://example.com/linux.tar.gz"},
                {"name": "tool-darwin.tar.gz", "browser_download_url": "https://example.com/darwin.tar.gz"}
            ]
        }"#;

        let release: Release = serde_json::from_str(json).unwrap();
        assert_eq!(release.tag_name, "v1.0.0");
        assert_eq!(release.assets.len(), 2);
        assert_eq!(release.assets[0].name, "tool-linux.tar.gz");
        assert_eq!(
            release.assets[0].browser_download_url,
            "https://example.com/linux.tar.gz"
        );
    }

    #[test]
    fn test_release_deserialization_empty_assets() {
        let json = r#"{"tag_name": "v0.1.0", "assets": []}"#;
        let release: Release = serde_json::from_str(json).unwrap();
        assert!(release.assets.is_empty());
    }

    // ==========================================================================
    // format_reset_duration tests for various time periods
    // ==========================================================================

    #[test]
    fn test_rate_limit_info_format_reset_duration_seconds() {
        // Future timestamp - current time + 45 seconds
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let info = RateLimitInfo {
            limit: None,
            remaining: None,
            reset: Some(now + 45),
        };
        let duration = info.format_reset_duration();
        assert!(duration.is_some());
        assert!(duration.unwrap().contains("second(s)"));
    }

    #[test]
    fn test_rate_limit_info_format_reset_duration_minutes() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let info = RateLimitInfo {
            limit: None,
            remaining: None,
            reset: Some(now + 300), // 5 minutes
        };
        let duration = info.format_reset_duration();
        assert!(duration.is_some());
        assert!(duration.unwrap().contains("minute(s)"));
    }

    #[test]
    fn test_rate_limit_info_format_reset_duration_hours() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let info = RateLimitInfo {
            limit: None,
            remaining: None,
            reset: Some(now + 7200), // 2 hours
        };
        let duration = info.format_reset_duration();
        assert!(duration.is_some());
        assert!(duration.unwrap().contains("hour(s)"));
    }

    // ==========================================================================
    // extract_binary tests
    // ==========================================================================

    #[test]
    fn test_extract_binary_raw_binary() {
        let provider = GitHubToolProvider::new();
        let temp_dir = TempDir::new().unwrap();

        let data = b"#!/bin/sh\necho hello\n";
        let result = provider.extract_binary(data, "mytool", None, temp_dir.path());

        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.exists());
        assert!(path.file_name().unwrap().to_string_lossy().contains("mytool"));
    }

    #[test]
    fn test_extract_binary_raw_binary_with_extension() {
        let provider = GitHubToolProvider::new();
        let temp_dir = TempDir::new().unwrap();

        let data = b"binary content here";
        let result = provider.extract_binary(data, "tool.exe", None, temp_dir.path());

        assert!(result.is_ok());
        let path = result.unwrap();
        // Should strip extension when naming the binary
        assert!(path.file_name().unwrap().to_string_lossy() == "tool");
    }

    // ==========================================================================
    // find_main_binary tests
    // ==========================================================================

    #[test]
    fn test_find_main_binary_in_bin_dir() {
        let provider = GitHubToolProvider::new();
        let temp_dir = TempDir::new().unwrap();

        // Create bin directory with executable
        let bin_dir = temp_dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let binary_path = bin_dir.join("tool");
        std::fs::write(&binary_path, b"binary").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&binary_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&binary_path, perms).unwrap();
        }

        let result = provider.find_main_binary(temp_dir.path());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), binary_path);
    }

    #[test]
    fn test_find_main_binary_in_root() {
        let provider = GitHubToolProvider::new();
        let temp_dir = TempDir::new().unwrap();

        // Create executable in root
        let binary_path = temp_dir.path().join("tool");
        std::fs::write(&binary_path, b"binary").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&binary_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&binary_path, perms).unwrap();
        }

        let result = provider.find_main_binary(temp_dir.path());
        assert!(result.is_ok());
    }

    #[test]
    fn test_find_main_binary_empty_dir() {
        let provider = GitHubToolProvider::new();
        let temp_dir = TempDir::new().unwrap();

        let result = provider.find_main_binary(temp_dir.path());
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("No binary found"));
    }

    #[test]
    fn test_find_main_binary_empty_bin_dir_fallback_to_root() {
        let provider = GitHubToolProvider::new();
        let temp_dir = TempDir::new().unwrap();

        // Create empty bin directory
        std::fs::create_dir_all(temp_dir.path().join("bin")).unwrap();

        // Create executable in root
        let binary_path = temp_dir.path().join("tool");
        std::fs::write(&binary_path, b"binary").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&binary_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&binary_path, perms).unwrap();
        }

        let result = provider.find_main_binary(temp_dir.path());
        assert!(result.is_ok());
    }

    // ==========================================================================
    // compute_file_sha256 tests
    // ==========================================================================

    #[tokio::test]
    async fn test_compute_file_sha256() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.bin");
        std::fs::write(&file_path, b"hello world").unwrap();

        let sha256 = compute_file_sha256(&file_path).await.unwrap();

        // SHA256 of "hello world" is known
        assert_eq!(
            sha256,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[tokio::test]
    async fn test_compute_file_sha256_empty_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("empty.bin");
        std::fs::write(&file_path, b"").unwrap();

        let sha256 = compute_file_sha256(&file_path).await.unwrap();

        // SHA256 of empty string
        assert_eq!(
            sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[tokio::test]
    async fn test_compute_file_sha256_large_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("large.bin");

        // Create a file larger than the buffer size (8192)
        let data = vec![0u8; 10000];
        std::fs::write(&file_path, &data).unwrap();

        let result = compute_file_sha256(&file_path).await;
        assert!(result.is_ok());
        // Just verify it's a valid hex string
        assert_eq!(result.unwrap().len(), 64);
    }

    // ==========================================================================
    // ToolSource variant tests
    // ==========================================================================

    #[test]
    fn test_can_handle_oci_source() {
        let provider = GitHubToolProvider::new();

        let oci_source = ToolSource::Oci {
            image: "docker.io/library/alpine:latest".into(),
            path: "/bin/sh".into(),
        };
        assert!(!provider.can_handle(&oci_source));
    }

    #[test]
    fn test_can_handle_rustup_source() {
        let provider = GitHubToolProvider::new();

        let rustup_source = ToolSource::Rustup {
            toolchain: "stable".into(),
            profile: Some("minimal".into()),
            components: vec![],
            targets: vec![],
        };
        assert!(!provider.can_handle(&rustup_source));
    }

    // ==========================================================================
    // Additional edge case tests
    // ==========================================================================

    #[test]
    fn test_expand_template_multiple_same_placeholder() {
        let provider = GitHubToolProvider::new();
        let platform = Platform::new(Os::Linux, Arch::X86_64);

        let result =
            provider.expand_template("{os}-{os}-{arch}-{arch}", "1.0.0", &platform);
        assert_eq!(result, "linux-linux-x86_64-x86_64");
    }

    #[test]
    fn test_expand_template_version_at_multiple_positions() {
        let provider = GitHubToolProvider::new();
        let platform = Platform::new(Os::Darwin, Arch::Arm64);

        let result = provider.expand_template("v{version}/tool-{version}.zip", "2.0.0", &platform);
        assert_eq!(result, "v2.0.0/tool-2.0.0.zip");
    }

    #[test]
    fn test_tool_cache_dir_special_characters_in_name() {
        let provider = GitHubToolProvider::new();
        let temp_dir = TempDir::new().unwrap();
        let options = ToolOptions::new().with_cache_dir(temp_dir.path().to_path_buf());

        // Tool names with hyphens and underscores
        let cache_dir = provider.tool_cache_dir(&options, "my-cool_tool", "1.0.0-beta.1");
        assert!(cache_dir.ends_with("github/my-cool_tool/1.0.0-beta.1"));
    }

    #[test]
    fn test_rate_limit_info_debug() {
        let info = RateLimitInfo {
            limit: Some(60),
            remaining: Some(30),
            reset: Some(1_735_689_600),
        };
        let debug = format!("{:?}", info);
        assert!(debug.contains("RateLimitInfo"));
        assert!(debug.contains("60"));
        assert!(debug.contains("30"));
    }

    #[test]
    fn test_asset_debug() {
        let asset = Asset {
            name: "tool.tar.gz".to_string(),
            browser_download_url: "https://example.com/tool.tar.gz".to_string(),
        };
        let debug = format!("{:?}", asset);
        assert!(debug.contains("Asset"));
        assert!(debug.contains("tool.tar.gz"));
    }

    #[test]
    fn test_release_debug() {
        let release = Release {
            tag_name: "v1.0.0".to_string(),
            assets: vec![],
        };
        let debug = format!("{:?}", release);
        assert!(debug.contains("Release"));
        assert!(debug.contains("v1.0.0"));
    }
}
