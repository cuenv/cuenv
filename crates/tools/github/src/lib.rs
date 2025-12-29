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

    /// Fetch release information from GitHub API.
    async fn fetch_release(&self, repo: &str, tag: &str) -> Result<Release> {
        let url = format!(
            "https://api.github.com/repos/{}/releases/tags/{}",
            repo, tag
        );
        debug!(%url, "Fetching GitHub release");

        let mut request = self.client.get(&url);

        // Add auth token if available
        if let Ok(token) = std::env::var("GITHUB_TOKEN") {
            request = request.header("Authorization", format!("Bearer {}", token));
        } else if let Ok(token) = std::env::var("GH_TOKEN") {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request.send().await.map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to fetch release: {}", e))
        })?;

        if !response.status().is_success() {
            return Err(cuenv_core::Error::tool_resolution(format!(
                "Release not found: {} {} (HTTP {})",
                repo,
                tag,
                response.status()
            )));
        }

        response.json().await.map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to parse release: {}", e))
        })
    }

    /// Download an asset from GitHub.
    async fn download_asset(&self, url: &str) -> Result<Vec<u8>> {
        debug!(%url, "Downloading GitHub asset");

        let mut request = self.client.get(url);

        // Add auth token if available
        if let Ok(token) = std::env::var("GITHUB_TOKEN") {
            request = request.header("Authorization", format!("Bearer {}", token));
        } else if let Ok(token) = std::env::var("GH_TOKEN") {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request.send().await.map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to download asset: {}", e))
        })?;

        if !response.status().is_success() {
            return Err(cuenv_core::Error::tool_resolution(format!(
                "Failed to download asset (HTTP {})",
                response.status()
            )));
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
            .unwrap_or("v{version}");

        let path = config.get("path").and_then(|v| v.as_str());

        info!(%tool_name, %repo, %version, %platform, "Resolving GitHub release");

        // Expand templates
        let tag = self.expand_template(tag_template, version, platform);
        let asset = self.expand_template(asset_template, version, platform);

        // Fetch release to verify it exists
        let release = self.fetch_release(repo, &tag).await?;

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
                path: path.map(String::from),
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

        // Fetch release and download asset
        let release = self.fetch_release(repo, tag).await?;
        let found_asset = release
            .assets
            .iter()
            .find(|a| &a.name == asset)
            .ok_or_else(|| {
                cuenv_core::Error::tool_resolution(format!("Asset '{}' not found", asset))
            })?;

        let data = self
            .download_asset(&found_asset.browser_download_url)
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
mod tests {
    use super::*;

    #[test]
    fn test_provider_name() {
        let provider = GitHubToolProvider::new();
        assert_eq!(provider.name(), "github");
    }

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
    fn test_can_handle() {
        let provider = GitHubToolProvider::new();

        let github_source = ToolSource::GitHub {
            repo: "org/repo".into(),
            tag: "v1".into(),
            asset: "file.zip".into(),
            path: None,
        };
        assert!(provider.can_handle(&github_source));

        let homebrew_source = ToolSource::Homebrew {
            formula: "jq".into(),
            image_ref: "ghcr.io/homebrew/core/jq:1.7.1".into(),
        };
        assert!(!provider.can_handle(&homebrew_source));
    }
}
