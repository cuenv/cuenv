//! GitHub Releases tool provider for cuenv.
//!
//! Fetches development tools from GitHub Releases. Supports:
//! - Template variables in asset names: `{version}`, `{os}`, `{arch}`
//! - Automatic archive extraction (zip, tar.gz, pkg)
//! - Path-based binary extraction from archives

use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_core::tools::{
    Arch, FetchedTool, Os, Platform, ResolvedTool, ToolExtract, ToolOptions, ToolProvider,
    ToolResolveRequest, ToolSource,
};
use flate2::read::GzDecoder;
use reqwest::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
#[cfg(target_os = "macos")]
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
use tar::Archive;
#[cfg(target_os = "macos")]
use tempfile::Builder;
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
        let is_pkg = asset_name.ends_with(".pkg");

        if is_zip {
            self.extract_from_zip(data, binary_path, dest)
        } else if is_tar_gz {
            self.extract_from_tar_gz(data, binary_path, dest)
        } else if is_pkg {
            self.extract_from_pkg(data, binary_path, dest)
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

    /// Extract from a macOS .pkg archive.
    #[cfg(target_os = "macos")]
    fn extract_from_pkg(
        &self,
        data: &[u8],
        binary_path: Option<&str>,
        dest: &std::path::Path,
    ) -> Result<PathBuf> {
        std::fs::create_dir_all(dest)?;

        let work_dir = Builder::new().prefix("cuenv-pkg-").tempdir().map_err(|e| {
            cuenv_core::Error::tool_resolution(format!(
                "Failed to create temporary directory for pkg extraction: {}",
                e
            ))
        })?;

        let pkg_path = work_dir.path().join("asset.pkg");
        std::fs::write(&pkg_path, data)?;

        let expanded_dir = work_dir.path().join("expanded");
        Self::run_command(
            Command::new("pkgutil")
                .arg("--expand")
                .arg(&pkg_path)
                .arg(&expanded_dir),
            "expand pkg archive",
        )?;

        let payloads = Self::collect_payload_files(&expanded_dir)?;
        if payloads.is_empty() {
            return Err(cuenv_core::Error::tool_resolution(
                "No payload files found in pkg archive".to_string(),
            ));
        }

        for (index, payload_path) in payloads.iter().enumerate() {
            let payload_dir = work_dir.path().join(format!("payload-{index}"));
            std::fs::create_dir_all(&payload_dir)?;

            let payload_file = File::open(payload_path)?;
            let payload_extract = Self::run_command(
                Command::new("cpio")
                    .args(["-idm", "--quiet"])
                    .current_dir(&payload_dir)
                    .stdin(Stdio::from(payload_file)),
                "extract pkg payload",
            );

            if let Err(error) = payload_extract {
                debug!(?payload_path, %error, "Skipping unreadable pkg payload");
                continue;
            }

            if let Some(path) = binary_path {
                if let Some(found) = Self::find_path_in_tree(&payload_dir, path)? {
                    return Self::copy_extracted_file(&found, dest, path);
                }
            } else if let Ok(found) = self.find_main_binary(&payload_dir) {
                return Self::copy_extracted_file(&found, dest, "binary");
            }
        }

        if let Some(path) = binary_path {
            return Err(cuenv_core::Error::tool_resolution(format!(
                "Binary '{}' not found in pkg payloads",
                path
            )));
        }

        Err(cuenv_core::Error::tool_resolution(
            "No executable found in pkg payloads".to_string(),
        ))
    }

    /// Extract from a macOS .pkg archive (unsupported on non-macOS hosts).
    #[cfg(not(target_os = "macos"))]
    fn extract_from_pkg(
        &self,
        _data: &[u8],
        _binary_path: Option<&str>,
        _dest: &std::path::Path,
    ) -> Result<PathBuf> {
        Err(cuenv_core::Error::tool_resolution(
            ".pkg extraction is only supported on macOS hosts".to_string(),
        ))
    }

    /// Copy a selected extracted file into the destination directory.
    #[cfg(target_os = "macos")]
    fn copy_extracted_file(source: &Path, dest: &Path, fallback_name: &str) -> Result<PathBuf> {
        std::fs::create_dir_all(dest)?;
        let file_name = source
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(fallback_name);
        let dest_path = dest.join(file_name);
        std::fs::copy(source, &dest_path)?;
        Self::ensure_executable(&dest_path)?;
        Ok(dest_path)
    }

    /// Run a process and map non-zero exits to tool-resolution errors.
    #[cfg(target_os = "macos")]
    fn run_command(command: &mut Command, action: &str) -> Result<()> {
        let status = command.status().map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to {}: {}", action, e))
        })?;

        if status.success() {
            Ok(())
        } else {
            Err(cuenv_core::Error::tool_resolution(format!(
                "Failed to {}: {}",
                action, status
            )))
        }
    }

    /// Recursively collect all `Payload` files from an expanded pkg directory.
    #[cfg(target_os = "macos")]
    fn collect_payload_files(root: &Path) -> Result<Vec<PathBuf>> {
        let mut stack = vec![root.to_path_buf()];
        let mut payloads = Vec::new();

        while let Some(current) = stack.pop() {
            for entry in std::fs::read_dir(&current)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }

                if path.is_file() && path.file_name().and_then(|n| n.to_str()) == Some("Payload") {
                    payloads.push(path);
                }
            }
        }

        Ok(payloads)
    }

    /// Find a file in a directory tree matching the requested pkg path.
    #[cfg(target_os = "macos")]
    fn find_path_in_tree(root: &Path, path: &str) -> Result<Option<PathBuf>> {
        let requested = Self::normalize_lookup_path(path);
        let mut stack = vec![root.to_path_buf()];

        while let Some(current) = stack.pop() {
            for entry in std::fs::read_dir(&current)? {
                let entry = entry?;
                let entry_path = entry.path();

                if entry_path.is_dir() {
                    stack.push(entry_path);
                    continue;
                }

                if !entry_path.is_file() {
                    continue;
                }

                let Ok(relative) = entry_path.strip_prefix(root) else {
                    continue;
                };
                let candidate = relative.to_string_lossy().replace('\\', "/");
                let candidate = candidate.trim_start_matches("./");

                if candidate == requested || candidate.ends_with(&format!("/{requested}")) {
                    return Ok(Some(entry_path));
                }
            }
        }

        Ok(None)
    }

    /// Normalize lookup paths for suffix matching.
    #[cfg(target_os = "macos")]
    fn normalize_lookup_path(path: &str) -> String {
        path.trim_start_matches('/')
            .trim_start_matches("./")
            .to_string()
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

    /// Ensure a file is executable on Unix hosts.
    fn ensure_executable(path: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms)?;
        }

        Ok(())
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
            extract: vec![],
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
            extract: vec![ToolExtract::Bin {
                path: "bin/tool".into(),
                as_name: None,
            }],
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
    // library path routing tests
    // ==========================================================================

    #[test]
    fn test_path_looks_like_library() {
        assert!(GitHubToolProvider::path_looks_like_library(
            "lib/libfdb_c.dylib"
        ));
        assert!(GitHubToolProvider::path_looks_like_library(
            "lib/libssl.so.3"
        ));
        assert!(GitHubToolProvider::path_looks_like_library(
            "bin/sqlite3.dll"
        ));
        assert!(!GitHubToolProvider::path_looks_like_library("bin/fdbcli"));
    }

    #[test]
    fn test_cache_targets_from_source_uses_lib_for_library_extract() {
        let provider = GitHubToolProvider::new();
        let temp_dir = TempDir::new().unwrap();
        let options = ToolOptions::new().with_cache_dir(temp_dir.path().to_path_buf());

        let resolved = ResolvedTool {
            name: "foundationdb".to_string(),
            version: "7.3.63".to_string(),
            platform: Platform::new(Os::Darwin, Arch::Arm64),
            source: ToolSource::GitHub {
                repo: "apple/foundationdb".to_string(),
                tag: "7.3.63".to_string(),
                asset: "FoundationDB-7.3.63_arm64.pkg".to_string(),
                extract: vec![ToolExtract::Lib {
                    path: "libfdb_c.dylib".to_string(),
                    env: None,
                }],
            },
        };

        let target = provider
            .cache_targets_from_source(&resolved, &options)
            .into_iter()
            .next()
            .unwrap();
        assert!(target.ends_with("github/foundationdb/7.3.63/lib/libfdb_c.dylib"));
    }

    #[test]
    fn test_cache_targets_from_source_uses_bin_for_default_extract() {
        let provider = GitHubToolProvider::new();
        let temp_dir = TempDir::new().unwrap();
        let options = ToolOptions::new().with_cache_dir(temp_dir.path().to_path_buf());

        let resolved = ResolvedTool {
            name: "foundationdb".to_string(),
            version: "7.3.63".to_string(),
            platform: Platform::new(Os::Darwin, Arch::Arm64),
            source: ToolSource::GitHub {
                repo: "apple/foundationdb".to_string(),
                tag: "7.3.63".to_string(),
                asset: "FoundationDB-7.3.63_arm64.pkg".to_string(),
                extract: vec![],
            },
        };
        let target = provider
            .cache_targets_from_source(&resolved, &options)
            .into_iter()
            .next()
            .unwrap();
        assert!(target.ends_with("github/foundationdb/7.3.63/bin/foundationdb"));
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
                extract: vec![],
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
                extract: vec![],
            },
        };

        // Create the cached file
        let cache_dir = provider.tool_cache_dir(&options, "mytool", "1.0.0");
        let bin_dir = cache_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(bin_dir.join("mytool"), b"binary").unwrap();

        assert!(provider.is_cached(&resolved, &options));
    }

    #[test]
    fn test_is_cached_library_path_uses_lib_directory() {
        let provider = GitHubToolProvider::new();
        let temp_dir = TempDir::new().unwrap();
        let options = ToolOptions::new().with_cache_dir(temp_dir.path().to_path_buf());

        let resolved = ResolvedTool {
            name: "foundationdb".to_string(),
            version: "7.3.63".to_string(),
            platform: Platform::new(Os::Darwin, Arch::Arm64),
            source: ToolSource::GitHub {
                repo: "apple/foundationdb".to_string(),
                tag: "7.3.63".to_string(),
                asset: "FoundationDB-7.3.63_arm64.pkg".to_string(),
                extract: vec![ToolExtract::Lib {
                    path: "libfdb_c.dylib".to_string(),
                    env: None,
                }],
            },
        };

        let cache_dir = provider.tool_cache_dir(&options, "foundationdb", "7.3.63");
        let lib_dir = cache_dir.join("lib");
        std::fs::create_dir_all(&lib_dir).unwrap();
        std::fs::write(lib_dir.join("libfdb_c.dylib"), b"library").unwrap();

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
}
