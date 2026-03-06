//! URL tool provider for cuenv.
//!
//! Downloads development tools from arbitrary HTTP/HTTPS URLs. Supports:
//! - Template variables in URLs: `{version}`, `{os}`, `{arch}`
//! - Automatic archive extraction (zip, tar.gz)
//! - Path-based binary extraction from archives
//! - Typed extraction rules (bin, lib, include, pkgconfig, file)

use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_core::tools::{
    Arch, FetchedTool, Os, Platform, ResolvedTool, ToolExtract, ToolOptions, ToolProvider,
    ToolResolveRequest, ToolSource,
};
use flate2::read::GzDecoder;
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use tar::Archive;
use tokio::io::AsyncReadExt;
use tracing::{debug, info};

/// Tool provider for arbitrary HTTP/HTTPS URLs.
///
/// Downloads binaries or archives from any URL, supporting template expansion
/// for platform-specific asset names and paths.
pub struct UrlToolProvider {
    client: Client,
}

impl Default for UrlToolProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl UrlToolProvider {
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
                        "Failed to create URL HTTP client: primary={primary_err}; fallback={fallback_err}"
                    )
                }),
            Err(_) => Client::builder()
                .user_agent("cuenv")
                .no_proxy()
                .build()
                .expect("Failed to create URL HTTP client after system proxy discovery panicked"),
        }
    }

    /// Create a new URL tool provider.
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: Self::build_client(),
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

        let response = self.client.get(url).send().await.map_err(|e| {
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

        response
            .bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| {
                cuenv_core::Error::tool_resolution(format!(
                    "Failed to read response from '{}': {}",
                    url, e
                ))
            })
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

    /// Determine whether a path string looks like a dynamic library.
    fn path_looks_like_library(path: &str) -> bool {
        let path_lower = path.to_ascii_lowercase();
        path_lower.ends_with(".dylib")
            || path_lower.ends_with(".so")
            || path_lower.contains(".so.")
            || path_lower.ends_with(".dll")
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

    /// Extract a binary from an archive or treat the download as a raw binary.
    fn extract_binary(
        &self,
        data: &[u8],
        url: &str,
        binary_path: Option<&str>,
        dest: &Path,
    ) -> Result<PathBuf> {
        // Infer archive type from the URL path
        let url_path = url.split('?').next().unwrap_or(url);
        let is_zip = url_path.ends_with(".zip");
        let is_tar_gz = url_path.ends_with(".tar.gz") || url_path.ends_with(".tgz");

        if is_zip {
            self.extract_from_zip(data, binary_path, dest)
        } else if is_tar_gz {
            self.extract_from_tar_gz(data, binary_path, dest)
        } else {
            // Assume it's a raw binary
            std::fs::create_dir_all(dest)?;
            let binary_name = std::path::Path::new(url_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("tool");
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
    fn extract_from_zip(
        &self,
        data: &[u8],
        binary_path: Option<&str>,
        dest: &Path,
    ) -> Result<PathBuf> {
        let cursor = Cursor::new(data);
        let mut archive = zip::ZipArchive::new(cursor).map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to open zip: {}", e))
        })?;

        // If a specific path is requested, extract just that file
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
                "Binary '{}' not found in zip archive",
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

        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir)?;
        }
        std::fs::create_dir_all(&temp_dir)?;

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

        if let Err(e) = extract_result {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err(e);
        }

        if dest.exists() {
            std::fs::remove_dir_all(dest)?;
        }
        std::fs::rename(&temp_dir, dest)?;

        self.find_main_binary(dest)
    }

    /// Extract from a tar.gz archive.
    fn extract_from_tar_gz(
        &self,
        data: &[u8],
        binary_path: Option<&str>,
        dest: &Path,
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
                "Binary '{}' not found in tar.gz archive",
                path
            )));
        }

        // Extract all files
        archive.unpack(dest).map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to extract tar: {}", e))
        })?;

        self.find_main_binary(dest)
    }

    /// Find the main binary in an extracted directory.
    fn find_main_binary(&self, dir: &Path) -> Result<PathBuf> {
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
                    return Ok(path);
                }
            }
        }

        Err(cuenv_core::Error::tool_resolution(
            "No binary found in extracted archive".to_string(),
        ))
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
            let extracted = self.extract_binary(&data, url, None, &cache_dir)?;
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
                self.extract_binary(&data, url, Some(source_path), &extract_dir)?;
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
mod tests {
    use super::*;
    use cuenv_core::tools::{Arch, Os, Platform};

    #[test]
    fn test_expand_template_version() {
        let result =
            UrlToolProvider::expand_template("https://example.com/tool-{version}.tar.gz", "1.2.3", &Platform::new(Os::Linux, Arch::X86_64));
        assert_eq!(result, "https://example.com/tool-1.2.3.tar.gz");
    }

    #[test]
    fn test_expand_template_os_linux() {
        let result = UrlToolProvider::expand_template(
            "https://example.com/tool-{os}.tar.gz",
            "1.0.0",
            &Platform::new(Os::Linux, Arch::X86_64),
        );
        assert_eq!(result, "https://example.com/tool-linux.tar.gz");
    }

    #[test]
    fn test_expand_template_os_darwin() {
        let result = UrlToolProvider::expand_template(
            "https://example.com/tool-{os}.tar.gz",
            "1.0.0",
            &Platform::new(Os::Darwin, Arch::Arm64),
        );
        assert_eq!(result, "https://example.com/tool-darwin.tar.gz");
    }

    #[test]
    fn test_expand_template_arch_x86_64() {
        let result = UrlToolProvider::expand_template(
            "https://example.com/tool-{arch}.tar.gz",
            "1.0.0",
            &Platform::new(Os::Linux, Arch::X86_64),
        );
        assert_eq!(result, "https://example.com/tool-x86_64.tar.gz");
    }

    #[test]
    fn test_expand_template_arch_arm64() {
        let result = UrlToolProvider::expand_template(
            "https://example.com/tool-{arch}.tar.gz",
            "1.0.0",
            &Platform::new(Os::Linux, Arch::Arm64),
        );
        assert_eq!(result, "https://example.com/tool-aarch64.tar.gz");
    }

    #[test]
    fn test_expand_template_all() {
        let result = UrlToolProvider::expand_template(
            "https://example.com/{version}/{os}/{arch}/tool.tar.gz",
            "2.0.0",
            &Platform::new(Os::Darwin, Arch::Arm64),
        );
        assert_eq!(
            result,
            "https://example.com/2.0.0/darwin/aarch64/tool.tar.gz"
        );
    }

    #[test]
    fn test_path_looks_like_library() {
        assert!(UrlToolProvider::path_looks_like_library("libfoo.so"));
        assert!(UrlToolProvider::path_looks_like_library("libfoo.dylib"));
        assert!(UrlToolProvider::path_looks_like_library("foo.dll"));
        assert!(UrlToolProvider::path_looks_like_library("libfoo.so.1"));
        assert!(!UrlToolProvider::path_looks_like_library("foo"));
        assert!(!UrlToolProvider::path_looks_like_library("foo.tar.gz"));
    }

    #[test]
    fn test_provider_name() {
        let provider = UrlToolProvider::new();
        assert_eq!(provider.name(), "url");
    }

    #[test]
    fn test_can_handle_url_source() {
        let provider = UrlToolProvider::new();
        let source = ToolSource::Url {
            url: "https://example.com/tool".to_string(),
            extract: vec![],
        };
        assert!(provider.can_handle(&source));
    }

    #[test]
    fn test_cannot_handle_github_source() {
        let provider = UrlToolProvider::new();
        let source = ToolSource::GitHub {
            repo: "owner/repo".to_string(),
            tag: "v1.0.0".to_string(),
            asset: "tool.tar.gz".to_string(),
            extract: vec![],
        };
        assert!(!provider.can_handle(&source));
    }

    #[tokio::test]
    async fn test_resolve_simple_url() {
        let provider = UrlToolProvider::new();
        let config = serde_json::json!({
            "type": "url",
            "url": "https://example.com/tool-{version}-{os}-{arch}.tar.gz"
        });
        let platform = Platform::new(Os::Linux, Arch::X86_64);
        let request = ToolResolveRequest {
            tool_name: "mytool",
            version: "1.0.0",
            platform: &platform,
            config: &config,
            token: None,
        };

        let resolved = provider.resolve(&request).await.unwrap();
        assert_eq!(resolved.name, "mytool");
        assert_eq!(resolved.version, "1.0.0");

        match &resolved.source {
            ToolSource::Url { url, extract } => {
                assert_eq!(url, "https://example.com/tool-1.0.0-linux-x86_64.tar.gz");
                assert!(extract.is_empty());
            }
            _ => panic!("Expected URL source"),
        }
    }

    #[tokio::test]
    async fn test_resolve_url_with_path() {
        let provider = UrlToolProvider::new();
        let config = serde_json::json!({
            "type": "url",
            "url": "https://example.com/tool-{version}.tar.gz",
            "path": "tool-{version}/bin/tool"
        });
        let platform = Platform::new(Os::Linux, Arch::X86_64);
        let request = ToolResolveRequest {
            tool_name: "mytool",
            version: "2.0.0",
            platform: &platform,
            config: &config,
            token: None,
        };

        let resolved = provider.resolve(&request).await.unwrap();
        match &resolved.source {
            ToolSource::Url { url, extract } => {
                assert_eq!(url, "https://example.com/tool-2.0.0.tar.gz");
                assert_eq!(extract.len(), 1);
                match &extract[0] {
                    ToolExtract::Bin { path, .. } => {
                        assert_eq!(path, "tool-2.0.0/bin/tool");
                    }
                    _ => panic!("Expected Bin extract"),
                }
            }
            _ => panic!("Expected URL source"),
        }
    }

    #[tokio::test]
    async fn test_resolve_url_missing_url_field() {
        let provider = UrlToolProvider::new();
        let config = serde_json::json!({
            "type": "url"
        });
        let platform = Platform::new(Os::Linux, Arch::X86_64);
        let request = ToolResolveRequest {
            tool_name: "mytool",
            version: "1.0.0",
            platform: &platform,
            config: &config,
            token: None,
        };

        assert!(provider.resolve(&request).await.is_err());
    }
}
