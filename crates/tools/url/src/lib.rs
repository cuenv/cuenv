//! URL tool provider for cuenv.
//!
//! Downloads development tools from arbitrary HTTP/HTTPS URLs. Supports:
//! - Template variables in URLs: `{version}`, `{os}`, `{arch}`
//! - Automatic archive extraction (zip, tar.gz, tar.xz)
//! - Path-based binary extraction from archives
//! - Typed extraction rules (bin, lib, include, pkgconfig, file)

mod extract;

use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_core::http::ensure_rustls_crypto_provider;
use cuenv_core::tools::{
    Arch, FetchedTool, Os, Platform, ResolvedTool, ToolExtract, ToolOptions, ToolProvider,
    ToolResolveRequest, ToolSource,
};
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::io::AsyncReadExt;
use tracing::{debug, info};

/// Tool provider for arbitrary HTTP/HTTPS URLs.
///
/// Downloads binaries or archives from any URL, supporting template expansion
/// for platform-specific asset names and paths.
pub struct UrlToolProvider {
    client: OnceLock<std::result::Result<Client, String>>,
}

impl Default for UrlToolProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl UrlToolProvider {
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
                        "Failed to create URL HTTP client: primary={primary_err}; fallback={fallback_err}"
                    )
                }),
            Err(_) => Client::builder()
                .user_agent("cuenv")
                .no_proxy()
                .build()
                .map_err(|fallback_err| {
                    format!(
                        "Failed to create URL HTTP client after system proxy discovery panicked: fallback={fallback_err}"
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

    /// Create a new URL tool provider.
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: OnceLock::new(),
        }
    }

    /// Get the cache directory for a tool.
    fn tool_cache_dir(&self, options: &ToolOptions, name: &str, version: &str) -> PathBuf {
        options.cache_dir().join("url").join(name).join(version)
    }

    /// Expand template variables in a string.
    fn expand_template(template: &str, version: &str, platform: &Platform) -> String {
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

    /// Download a resource from a URL.
    async fn download_url(&self, url: &str) -> Result<Vec<u8>> {
        debug!(%url, "Downloading URL asset");
        let client = self.client()?;

        let response = client.get(url).send().await.map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to download from '{}': {}", url, e))
        })?;

        let status = response.status();
        if !status.is_success() {
            if status == reqwest::StatusCode::NOT_FOUND {
                return Err(cuenv_core::Error::tool_resolution(format!(
                    "URL '{}' not found (HTTP 404)",
                    url
                )));
            }
            return Err(cuenv_core::Error::tool_resolution(format!(
                "Failed to download from '{}': HTTP {}",
                url, status
            )));
        }

        response.bytes().await.map(|b| b.to_vec()).map_err(|e| {
            cuenv_core::Error::tool_resolution(format!(
                "Failed to read response from '{}': {}",
                url, e
            ))
        })
    }

    fn cache_targets_from_source(
        &self,
        resolved: &ResolvedTool,
        options: &ToolOptions,
    ) -> Vec<PathBuf> {
        let cache_dir = self.tool_cache_dir(options, &resolved.name, &resolved.version);
        let extract = match &resolved.source {
            ToolSource::Url { extract, .. } => extract,
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

    fn expand_extract_templates(
        extract: &[ToolExtract],
        version: &str,
        platform: &Platform,
    ) -> Vec<ToolExtract> {
        extract
            .iter()
            .map(|item| match item {
                ToolExtract::Bin { path, as_name } => ToolExtract::Bin {
                    path: Self::expand_template(path, version, platform),
                    as_name: as_name.clone(),
                },
                ToolExtract::Lib { path, env } => ToolExtract::Lib {
                    path: Self::expand_template(path, version, platform),
                    env: env.clone(),
                },
                ToolExtract::Include { path } => ToolExtract::Include {
                    path: Self::expand_template(path, version, platform),
                },
                ToolExtract::PkgConfig { path } => ToolExtract::PkgConfig {
                    path: Self::expand_template(path, version, platform),
                },
                ToolExtract::File { path, env } => ToolExtract::File {
                    path: Self::expand_template(path, version, platform),
                    env: env.clone(),
                },
            })
            .collect()
    }
}

#[async_trait]
impl ToolProvider for UrlToolProvider {
    fn name(&self) -> &'static str {
        "url"
    }

    fn description(&self) -> &'static str {
        "Fetch tools from arbitrary HTTP/HTTPS URLs"
    }

    fn can_handle(&self, source: &ToolSource) -> bool {
        matches!(source, ToolSource::Url { .. })
    }

    async fn resolve(&self, request: &ToolResolveRequest<'_>) -> Result<ResolvedTool> {
        let tool_name = request.tool_name;
        let version = request.version;
        let platform = request.platform;
        let config = request.config;

        let url_template = config
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| cuenv_core::Error::tool_resolution("Missing 'url' in config"))?;

        let extract: Vec<ToolExtract> = config
            .get("extract")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| {
                cuenv_core::Error::tool_resolution(format!(
                    "Invalid 'extract' in URL source config: {}",
                    e
                ))
            })?
            .unwrap_or_default();

        let path = config
            .get("path")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Expand templates in URL
        let url = Self::expand_template(url_template, version, platform);

        // Build extract list: expand templates, fall back to legacy path
        let mut expanded_extract = Self::expand_extract_templates(&extract, version, platform);
        if expanded_extract.is_empty()
            && let Some(path) = path.as_deref()
        {
            let expanded_path = Self::expand_template(path, version, platform);
            if extract::path_looks_like_library(&expanded_path) {
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

        info!(%tool_name, %url, %version, %platform, "Resolving URL tool");

        Ok(ResolvedTool {
            name: tool_name.to_string(),
            version: version.to_string(),
            platform: platform.clone(),
            source: ToolSource::Url {
                url,
                extract: expanded_extract,
            },
        })
    }

    async fn fetch(&self, resolved: &ResolvedTool, options: &ToolOptions) -> Result<FetchedTool> {
        let ToolSource::Url { url, extract } = &resolved.source else {
            return Err(cuenv_core::Error::tool_resolution(
                "UrlToolProvider received non-URL source".to_string(),
            ));
        };

        info!(
            tool = %resolved.name,
            %url,
            "Fetching URL tool"
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

        let data = self.download_url(url).await?;

        if extract.is_empty() {
            // Legacy behavior: single binary inferred from archive or raw file.
            let extracted = extract::extract_binary(&data, url, None, &cache_dir)?;
            if extract::looks_like_prefix_install(&cache_dir) {
                let primary_path =
                    extract::find_primary_binary_in_prefix(&cache_dir, &resolved.name)?;
                let sha256 = compute_file_sha256(&primary_path).await?;
                info!(
                    tool = %resolved.name,
                    binary = ?primary_path,
                    %sha256,
                    "Fetched URL tool"
                );
                return Ok(FetchedTool {
                    name: resolved.name.clone(),
                    binary_path: primary_path,
                    sha256,
                });
            }
            let final_path = if extract::file_looks_like_library(&extracted) {
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
            if !extract::file_looks_like_library(&final_path) {
                extract::ensure_executable(&final_path)?;
            }

            let sha256 = compute_file_sha256(&final_path).await?;
            info!(
                tool = %resolved.name,
                binary = ?final_path,
                %sha256,
                "Fetched URL tool"
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
                extract::extract_binary(&data, url, Some(source_path), &extract_dir)?;
            let final_path = self.cache_target_for_extract(&cache_dir, &resolved.name, item);
            if let Some(parent) = final_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if final_path.exists() {
                std::fs::remove_file(&final_path)?;
            }
            std::fs::rename(&extracted_path, &final_path)?;
            if Self::is_executable_extract(item) {
                extract::ensure_executable(&final_path)?;
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
            "Fetched URL tool"
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

#[cfg(test)]
mod tests;
