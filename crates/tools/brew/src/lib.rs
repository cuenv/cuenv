//! Homebrew bottle tool provider for cuenv.
//!
//! Downloads pre-built binaries from Homebrew bottles hosted on ghcr.io.
//! Supports:
//! - Formula metadata from the Homebrew JSON API
//! - Platform-specific bottle selection
//! - Anonymous ghcr.io token acquisition for blob downloads
//! - SHA256 verification of downloaded bottles
//! - Binary extraction from gzip tarballs

use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_core::tools::{
    Arch, FetchedTool, Os, Platform, ResolvedTool, ToolOptions, ToolProvider, ToolResolveRequest,
    ToolSource,
};
use flate2::read::GzDecoder;
use reqwest::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use tar::Archive;
use tokio::io::AsyncReadExt;
use tracing::{debug, info};

/// Homebrew formula API response (partial).
#[derive(Debug, Deserialize)]
struct FormulaInfo {
    versions: FormulaVersions,
    bottle: BottleInfo,
}

/// Formula version information.
#[derive(Debug, Deserialize)]
struct FormulaVersions {
    stable: String,
}

/// Bottle information from the formula API.
#[derive(Debug, Deserialize)]
struct BottleInfo {
    stable: BottleStable,
}

/// Stable bottle information.
#[derive(Debug, Deserialize)]
struct BottleStable {
    files: serde_json::Value,
}

/// Individual bottle file entry.
#[derive(Debug, Deserialize)]
struct BottleFile {
    url: String,
    sha256: String,
}

/// Anonymous ghcr.io token response.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    token: String,
}

/// Tool provider for Homebrew bottles.
///
/// Fetches pre-built binaries from Homebrew bottles hosted on ghcr.io,
/// supporting platform-specific bottle selection and SHA256 verification.
pub struct BrewToolProvider {
    client: Client,
}

impl Default for BrewToolProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl BrewToolProvider {
    fn build_client() -> Client {
        let primary = catch_unwind(AssertUnwindSafe(|| {
            Client::builder().user_agent("cuenv").build()
        }));

        match primary {
            Ok(Ok(client)) => client,
            Ok(Err(primary_err)) => Client::builder()
                .user_agent("cuenv")
                .no_proxy()
                .build()
                .unwrap_or_else(|fallback_err| {
                    panic!(
                        "Failed to create Brew HTTP client: primary={primary_err}; fallback={fallback_err}"
                    )
                }),
            Err(_) => Client::builder()
                .user_agent("cuenv")
                .no_proxy()
                .build()
                .expect(
                    "Failed to create Brew HTTP client after system proxy discovery panicked",
                ),
        }
    }

    /// Create a new Homebrew bottle tool provider.
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: Self::build_client(),
        }
    }

    /// Get the cache directory for a tool.
    fn tool_cache_dir(&self, options: &ToolOptions, name: &str, version: &str) -> PathBuf {
        options.cache_dir().join("brew").join(name).join(version)
    }

    /// Get the Homebrew bottle key candidates for a given platform.
    fn bottle_keys_for_platform(platform: &Platform) -> Vec<&'static str> {
        match (&platform.os, &platform.arch) {
            (Os::Darwin, Arch::Arm64) => {
                vec!["arm64_tahoe", "arm64_sequoia", "arm64_sonoma"]
            }
            (Os::Darwin, Arch::X86_64) => vec!["sequoia", "sonoma"],
            (Os::Linux, Arch::X86_64) => vec!["x86_64_linux"],
            (Os::Linux, Arch::Arm64) => vec!["arm64_linux"],
        }
    }

    /// Fetch formula information from the Homebrew API.
    async fn fetch_formula(&self, formula: &str) -> Result<FormulaInfo> {
        let url = format!("https://formulae.brew.sh/api/formula/{}.json", formula);
        debug!(%url, "Fetching Homebrew formula info");

        let response = self.client.get(&url).send().await.map_err(|e| {
            cuenv_core::Error::tool_resolution(format!(
                "Failed to fetch Homebrew formula '{}': {}",
                formula, e
            ))
        })?;

        let status = response.status();
        if !status.is_success() {
            if status == reqwest::StatusCode::NOT_FOUND {
                return Err(cuenv_core::Error::tool_resolution_with_help(
                    format!("Homebrew formula '{}' not found", formula),
                    "Check the formula name at https://formulae.brew.sh/",
                ));
            }
            return Err(cuenv_core::Error::tool_resolution(format!(
                "Failed to fetch Homebrew formula '{}': HTTP {}",
                formula, status
            )));
        }

        response.json().await.map_err(|e| {
            cuenv_core::Error::tool_resolution(format!(
                "Failed to parse Homebrew formula '{}': {}",
                formula, e
            ))
        })
    }

    /// Find the best bottle file for a platform.
    fn find_bottle_for_platform<'a>(
        files: &'a serde_json::Value,
        platform: &Platform,
    ) -> Result<(&'a str, BottleFile)> {
        let keys = Self::bottle_keys_for_platform(platform);
        let files_map = files.as_object().ok_or_else(|| {
            cuenv_core::Error::tool_resolution(
                "Homebrew bottle files is not an object".to_string(),
            )
        })?;

        for key in &keys {
            if let Some(entry) = files_map.get(*key) {
                let bottle_file: BottleFile = serde_json::from_value(entry.clone()).map_err(|e| {
                    cuenv_core::Error::tool_resolution(format!(
                        "Failed to parse bottle entry '{}': {}",
                        key, e
                    ))
                })?;
                // Return the key as a &str with lifetime tied to files
                // We need to get it from the map itself
                let key_ref = files_map
                    .keys()
                    .find(|k| k.as_str() == *key)
                    .map(String::as_str)
                    .unwrap_or(key);
                return Ok((key_ref, bottle_file));
            }
        }

        let available: Vec<&String> = files_map.keys().collect();
        Err(cuenv_core::Error::tool_resolution_with_help(
            format!(
                "No Homebrew bottle available for platform '{}'. Tried keys: {:?}",
                platform, keys
            ),
            format!("Available bottle platforms: {:?}", available),
        ))
    }

    /// Get an anonymous bearer token from ghcr.io.
    async fn get_ghcr_token(&self, formula: &str) -> Result<String> {
        let url = format!(
            "https://ghcr.io/token?service=ghcr.io&scope=repository:homebrew/core/{}:pull",
            formula
        );
        debug!(%url, "Fetching anonymous ghcr.io token");

        let response = self.client.get(&url).send().await.map_err(|e| {
            cuenv_core::Error::tool_resolution(format!(
                "Failed to get ghcr.io token for '{}': {}",
                formula, e
            ))
        })?;

        if !response.status().is_success() {
            return Err(cuenv_core::Error::tool_resolution(format!(
                "Failed to get ghcr.io token for '{}': HTTP {}",
                formula,
                response.status()
            )));
        }

        let token_response: TokenResponse = response.json().await.map_err(|e| {
            cuenv_core::Error::tool_resolution(format!(
                "Failed to parse ghcr.io token response: {}",
                e
            ))
        })?;

        Ok(token_response.token)
    }

    /// Download a bottle blob from ghcr.io with bearer authentication.
    async fn download_blob(&self, url: &str, token: &str) -> Result<Vec<u8>> {
        debug!(%url, "Downloading Homebrew bottle blob");

        let response = self
            .client
            .get(url)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .map_err(|e| {
                cuenv_core::Error::tool_resolution(format!(
                    "Failed to download bottle blob: {}",
                    e
                ))
            })?;

        if !response.status().is_success() {
            return Err(cuenv_core::Error::tool_resolution(format!(
                "Failed to download bottle blob: HTTP {}",
                response.status()
            )));
        }

        response.bytes().await.map(|b| b.to_vec()).map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to read bottle blob: {}", e))
        })
    }

    /// Verify SHA256 of downloaded data.
    fn verify_sha256(data: &[u8], expected: &str) -> Result<()> {
        let mut hasher = Sha256::new();
        hasher.update(data);
        let actual = format!("{:x}", hasher.finalize());

        if actual != expected {
            return Err(cuenv_core::Error::tool_resolution(format!(
                "SHA256 mismatch: expected {}, got {}",
                expected, actual
            )));
        }

        Ok(())
    }

    /// Extract a binary from a Homebrew bottle (gzip tarball).
    ///
    /// Homebrew bottles are tar.gz archives with entries at
    /// `{formula}/{version}/bin/{binary}` (or a custom path).
    fn extract_binary_from_bottle(
        data: &[u8],
        binary_path: &str,
        dest: &Path,
    ) -> Result<PathBuf> {
        let cursor = std::io::Cursor::new(data);
        let decoder = GzDecoder::new(cursor);
        let mut archive = Archive::new(decoder);

        std::fs::create_dir_all(dest)?;

        for entry in archive.entries().map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to read bottle tar: {}", e))
        })? {
            let mut entry = entry.map_err(|e| {
                cuenv_core::Error::tool_resolution(format!(
                    "Failed to read bottle tar entry: {}",
                    e
                ))
            })?;

            let entry_path = entry.path().map_err(|e| {
                cuenv_core::Error::tool_resolution(format!(
                    "Invalid path in bottle tar: {}",
                    e
                ))
            })?;

            let path_str = entry_path.to_string_lossy();
            if path_str.ends_with(binary_path) || path_str.as_ref() == binary_path {
                let file_name = Path::new(binary_path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(binary_path);
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

        Err(cuenv_core::Error::tool_resolution(format!(
            "Binary '{}' not found in Homebrew bottle archive",
            binary_path
        )))
    }
}

/// Compute SHA256 hash of a file.
async fn compute_file_sha256(path: &Path) -> Result<String> {
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

#[async_trait]
impl ToolProvider for BrewToolProvider {
    fn name(&self) -> &'static str {
        "brew"
    }

    fn description(&self) -> &'static str {
        "Fetch tools from Homebrew bottles on ghcr.io"
    }

    fn can_handle(&self, source: &ToolSource) -> bool {
        matches!(source, ToolSource::BrewBottle { .. })
    }

    async fn resolve(&self, request: &ToolResolveRequest<'_>) -> Result<ResolvedTool> {
        let tool_name = request.tool_name;
        let version = request.version;
        let platform = request.platform;
        let config = request.config;

        let formula = config
            .get("formula")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                cuenv_core::Error::tool_resolution("Missing 'formula' in brew config")
            })?;

        let custom_path = config.get("path").and_then(|v| v.as_str());

        info!(%tool_name, %formula, %version, %platform, "Resolving Homebrew bottle");

        // Fetch formula metadata
        let formula_info = self.fetch_formula(formula).await?;

        // Verify version matches
        if formula_info.versions.stable != version {
            debug!(
                expected = %version,
                actual = %formula_info.versions.stable,
                "Homebrew formula version mismatch - using requested version"
            );
        }

        // Find bottle for platform
        let (_bottle_key, bottle_file) =
            Self::find_bottle_for_platform(&formula_info.bottle.stable.files, platform)?;

        // Determine the path inside the bottle
        let path = custom_path
            .map(String::from)
            .unwrap_or_else(|| format!("bin/{}", formula));

        debug!(
            %formula,
            url = %bottle_file.url,
            sha256 = %bottle_file.sha256,
            %path,
            "Resolved Homebrew bottle"
        );

        Ok(ResolvedTool {
            name: tool_name.to_string(),
            version: version.to_string(),
            platform: platform.clone(),
            source: ToolSource::BrewBottle {
                formula: formula.to_string(),
                url: bottle_file.url,
                sha256: bottle_file.sha256,
                path,
            },
        })
    }

    async fn fetch(&self, resolved: &ResolvedTool, options: &ToolOptions) -> Result<FetchedTool> {
        let ToolSource::BrewBottle {
            formula,
            url,
            sha256: expected_sha256,
            path,
        } = &resolved.source
        else {
            return Err(cuenv_core::Error::tool_resolution(
                "BrewToolProvider received non-BrewBottle source".to_string(),
            ));
        };

        info!(
            tool = %resolved.name,
            %formula,
            %path,
            "Fetching Homebrew bottle"
        );

        // Check cache
        let cache_dir = self.tool_cache_dir(options, &resolved.name, &resolved.version);
        let binary_name = Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&resolved.name);
        let cached_path = cache_dir.join("bin").join(binary_name);

        if !options.force_refetch && cached_path.exists() {
            debug!(?cached_path, "Tool already cached");
            let sha256 = compute_file_sha256(&cached_path).await?;
            return Ok(FetchedTool {
                name: resolved.name.clone(),
                binary_path: cached_path,
                sha256,
            });
        }

        // Get anonymous ghcr.io token
        let token = self.get_ghcr_token(formula).await?;

        // Download the bottle blob
        let data = self.download_blob(url, &token).await?;

        // Verify SHA256
        Self::verify_sha256(&data, expected_sha256)?;

        // Extract binary from the gzip tarball
        // Homebrew bottles have entries at {formula}/{version}/{path}
        let full_path = format!("{}/{}/{}", formula, resolved.version, path);
        let extract_dir = cache_dir.join(".extract");
        if extract_dir.exists() {
            std::fs::remove_dir_all(&extract_dir)?;
        }

        let extracted = Self::extract_binary_from_bottle(&data, &full_path, &extract_dir)?;

        // Move to final cache location
        let bin_dir = cache_dir.join("bin");
        std::fs::create_dir_all(&bin_dir)?;

        let final_path = bin_dir.join(binary_name);
        if final_path.exists() {
            std::fs::remove_file(&final_path)?;
        }
        std::fs::rename(&extracted, &final_path)?;

        // Clean up extract dir
        if extract_dir.exists() {
            let _ = std::fs::remove_dir_all(&extract_dir);
        }

        let sha256 = compute_file_sha256(&final_path).await?;
        info!(
            tool = %resolved.name,
            binary = ?final_path,
            %sha256,
            "Fetched Homebrew bottle"
        );

        Ok(FetchedTool {
            name: resolved.name.clone(),
            binary_path: final_path,
            sha256,
        })
    }

    fn is_cached(&self, resolved: &ResolvedTool, options: &ToolOptions) -> bool {
        let ToolSource::BrewBottle { path, .. } = &resolved.source else {
            return false;
        };

        let cache_dir = self.tool_cache_dir(options, &resolved.name, &resolved.version);
        let binary_name = Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&resolved.name);
        cache_dir.join("bin").join(binary_name).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::tools::Platform;

    #[test]
    fn test_bottle_keys_darwin_arm64() {
        let platform = Platform::new(Os::Darwin, Arch::Arm64);
        let keys = BrewToolProvider::bottle_keys_for_platform(&platform);
        assert_eq!(keys, vec!["arm64_tahoe", "arm64_sequoia", "arm64_sonoma"]);
    }

    #[test]
    fn test_bottle_keys_darwin_x86_64() {
        let platform = Platform::new(Os::Darwin, Arch::X86_64);
        let keys = BrewToolProvider::bottle_keys_for_platform(&platform);
        assert_eq!(keys, vec!["sequoia", "sonoma"]);
    }

    #[test]
    fn test_bottle_keys_linux_x86_64() {
        let platform = Platform::new(Os::Linux, Arch::X86_64);
        let keys = BrewToolProvider::bottle_keys_for_platform(&platform);
        assert_eq!(keys, vec!["x86_64_linux"]);
    }

    #[test]
    fn test_bottle_keys_linux_arm64() {
        let platform = Platform::new(Os::Linux, Arch::Arm64);
        let keys = BrewToolProvider::bottle_keys_for_platform(&platform);
        assert_eq!(keys, vec!["arm64_linux"]);
    }

    #[test]
    fn test_find_bottle_for_platform() {
        let files = serde_json::json!({
            "arm64_sequoia": {
                "url": "https://ghcr.io/v2/homebrew/core/jq/blobs/sha256:abc123",
                "sha256": "abc123"
            },
            "x86_64_linux": {
                "url": "https://ghcr.io/v2/homebrew/core/jq/blobs/sha256:def456",
                "sha256": "def456"
            }
        });

        let platform = Platform::new(Os::Darwin, Arch::Arm64);
        let result = BrewToolProvider::find_bottle_for_platform(&files, &platform);
        assert!(result.is_ok());
        let (_key, bottle) = result.unwrap_or_else(|e| panic!("unexpected error: {}", e));
        assert_eq!(bottle.sha256, "abc123");
    }

    #[test]
    fn test_find_bottle_for_platform_missing() {
        let files = serde_json::json!({
            "x86_64_linux": {
                "url": "https://ghcr.io/v2/homebrew/core/jq/blobs/sha256:def456",
                "sha256": "def456"
            }
        });

        let platform = Platform::new(Os::Darwin, Arch::Arm64);
        let result = BrewToolProvider::find_bottle_for_platform(&files, &platform);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_sha256_valid() {
        let data = b"hello world";
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(BrewToolProvider::verify_sha256(data, expected).is_ok());
    }

    #[test]
    fn test_verify_sha256_invalid() {
        let data = b"hello world";
        let expected = "0000000000000000000000000000000000000000000000000000000000000000";
        assert!(BrewToolProvider::verify_sha256(data, expected).is_err());
    }

    #[test]
    fn test_provider_name() {
        let provider = BrewToolProvider::new();
        assert_eq!(provider.name(), "brew");
    }

    #[test]
    fn test_can_handle() {
        let provider = BrewToolProvider::new();

        let brew_source = ToolSource::BrewBottle {
            formula: "jq".to_string(),
            url: "https://example.com".to_string(),
            sha256: "abc123".to_string(),
            path: "bin/jq".to_string(),
        };
        assert!(provider.can_handle(&brew_source));

        let github_source = ToolSource::GitHub {
            repo: "jqlang/jq".to_string(),
            tag: "jq-1.7.1".to_string(),
            asset: "jq-macos-arm64".to_string(),
            extract: vec![],
        };
        assert!(!provider.can_handle(&github_source));
    }

    #[test]
    fn test_is_cached_returns_false_for_wrong_source() {
        let provider = BrewToolProvider::new();
        let resolved = ResolvedTool {
            name: "jq".to_string(),
            version: "1.7.1".to_string(),
            platform: Platform::new(Os::Darwin, Arch::Arm64),
            source: ToolSource::GitHub {
                repo: "jqlang/jq".to_string(),
                tag: "jq-1.7.1".to_string(),
                asset: "jq-macos-arm64".to_string(),
                extract: vec![],
            },
        };
        let options = ToolOptions::new();
        assert!(!provider.is_cached(&resolved, &options));
    }

    #[test]
    fn test_tool_source_brew_serialization() {
        let source = ToolSource::BrewBottle {
            formula: "jq".to_string(),
            url: "https://ghcr.io/v2/homebrew/core/jq/blobs/sha256:abc".to_string(),
            sha256: "abc123".to_string(),
            path: "bin/jq".to_string(),
        };
        let json = serde_json::to_string(&source).unwrap_or_default();
        assert!(json.contains("\"type\":\"brew\""));
        assert!(json.contains("\"formula\":\"jq\""));
    }

    #[test]
    fn test_tool_source_brew_deserialization() {
        let json = r#"{"type":"brew","formula":"jq","url":"https://example.com","sha256":"abc123","path":"bin/jq"}"#;
        let source: ToolSource = serde_json::from_str(json).unwrap_or_else(|e| {
            panic!("deserialization failed: {}", e)
        });
        match source {
            ToolSource::BrewBottle {
                formula,
                url,
                sha256,
                path,
            } => {
                assert_eq!(formula, "jq");
                assert_eq!(url, "https://example.com");
                assert_eq!(sha256, "abc123");
                assert_eq!(path, "bin/jq");
            }
            _ => panic!("Expected BrewBottle source"),
        }
    }
}
