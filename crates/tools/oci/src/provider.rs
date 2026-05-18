//! OCI tool provider implementation.
//!
//! Exposes [`OciToolProvider`], which implements [`ToolProvider`] for the
//! `ToolSource::Oci { image, path }` source kind. Resolves an image reference
//! to its platform-specific manifest digest, pulls the matching layers, and
//! extracts a single binary at the configured `path` to a content-addressed
//! cache directory.

use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_core::tools::{
    Arch as CoreArch, FetchedTool, Os as CoreOs, Platform as CorePlatform, ResolvedTool,
    ToolOptions, ToolProvider, ToolResolveRequest, ToolSource,
};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;
use tracing::{debug, info};

use crate::cache::OciCache;
use crate::extract::extract_from_layers;
use crate::platform::Platform as OciPlatform;
use crate::registry::OciClient;

/// Tool provider that extracts binaries from OCI container images.
///
/// Configuration shape (from CUE `#Oci`):
///
/// ```ignore
/// source: #Oci & {
///     image: "ghcr.io/owner/image:{version}"
///     path:  "/usr/local/bin/tool"
/// }
/// ```
///
/// Both `image` and `path` support `{version}`, `{os}`, and `{arch}` template
/// variables, mirroring the URL and GitHub providers.
pub struct OciToolProvider {
    client: OciClient,
}

impl Default for OciToolProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl OciToolProvider {
    /// Create a new OCI tool provider.
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: OciClient::new(),
        }
    }

    /// Per-tool, per-version, per-digest cache root.
    ///
    /// Binaries land at `<cache>/oci/<digest>/<binary_name>` so that the
    /// directory is addressable on PATH and downloads dedupe across tools
    /// that point at the same digest.
    fn binary_cache_dir(options: &ToolOptions, digest: &str) -> PathBuf {
        let sanitized = sanitize_digest(digest);
        options.cache_dir().join("oci").join(sanitized)
    }

    /// Expand `{version}`, `{os}`, `{arch}` placeholders.
    ///
    /// `{arch}` follows the same convention used by the URL and GitHub
    /// providers (`aarch64`, `x86_64`) so that templates are portable across
    /// providers. Image-index platform selection still uses the OCI
    /// convention (`arm64`, `amd64`) internally via [`Self::to_oci_platform`].
    fn expand_template(template: &str, version: &str, platform: &CorePlatform) -> String {
        let os_str = match platform.os {
            CoreOs::Darwin => "darwin",
            CoreOs::Linux => "linux",
        };
        let arch_str = match platform.arch {
            CoreArch::Arm64 => "aarch64",
            CoreArch::X86_64 => "x86_64",
        };

        template
            .replace("{version}", version)
            .replace("{os}", os_str)
            .replace("{arch}", arch_str)
    }

    /// Translate a core platform to the OCI registry platform shape used by
    /// the lower-level [`OciClient`].
    fn to_oci_platform(platform: &CorePlatform) -> OciPlatform {
        let os = match platform.os {
            CoreOs::Darwin => "darwin",
            CoreOs::Linux => "linux",
        };
        let arch = match platform.arch {
            CoreArch::Arm64 => "arm64",
            CoreArch::X86_64 => "x86_64",
        };
        OciPlatform::new(os, arch)
    }

    /// Derive the binary's filesystem name from the path inside the image.
    fn binary_name_for(path: &str) -> &str {
        Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("tool")
    }
}

#[async_trait]
impl ToolProvider for OciToolProvider {
    fn name(&self) -> &'static str {
        "oci"
    }

    fn description(&self) -> &'static str {
        "Extract binaries from OCI container images"
    }

    fn can_handle(&self, source: &ToolSource) -> bool {
        matches!(source, ToolSource::Oci { .. })
    }

    async fn resolve(&self, request: &ToolResolveRequest<'_>) -> Result<ResolvedTool> {
        let tool_name = request.tool_name;
        let version = request.version;
        let platform = request.platform;
        let config = request.config;

        let image_template = config
            .get("image")
            .and_then(|v| v.as_str())
            .ok_or_else(|| cuenv_core::Error::tool_resolution("Missing 'image' in OCI config"))?;
        let path_template = config
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| cuenv_core::Error::tool_resolution("Missing 'path' in OCI config"))?;

        let image = Self::expand_template(image_template, version, platform);
        let path = Self::expand_template(path_template, version, platform);

        info!(%tool_name, %image, %version, %platform, "Resolving OCI tool");

        Ok(ResolvedTool {
            name: tool_name.to_string(),
            version: version.to_string(),
            platform: platform.clone(),
            source: ToolSource::Oci { image, path },
        })
    }

    async fn fetch(&self, resolved: &ResolvedTool, options: &ToolOptions) -> Result<FetchedTool> {
        let ToolSource::Oci { image, path } = &resolved.source else {
            return Err(cuenv_core::Error::tool_resolution(
                "OciToolProvider received non-OCI source".to_string(),
            ));
        };

        let binary_name = Self::binary_name_for(path);
        let oci_platform = Self::to_oci_platform(&resolved.platform);

        info!(
            tool = %resolved.name,
            %image,
            %path,
            "Fetching OCI tool"
        );

        // Resolve manifest digest for the requested platform.
        let resolved_image = self
            .client
            .resolve_digest(image, &oci_platform)
            .await
            .map_err(|e| {
                cuenv_core::Error::tool_resolution(format!(
                    "Failed to resolve OCI image '{image}': {e}"
                ))
            })?;

        let cache_dir = Self::binary_cache_dir(options, &resolved_image.digest);
        let final_path = cache_dir.join(binary_name);

        if !options.force_refetch && final_path.exists() {
            debug!(?final_path, "Tool already cached");
            let sha256 = compute_file_sha256(&final_path).await?;
            return Ok(FetchedTool {
                name: resolved.name.clone(),
                binary_path: final_path,
                sha256,
            });
        }

        // Pull layers via the shared blob cache and extract.
        let oci_cache = OciCache::default();
        oci_cache.ensure_dirs().map_err(|e| {
            cuenv_core::Error::tool_resolution(format!(
                "Failed to prepare OCI cache directories: {e}"
            ))
        })?;

        let layer_paths = self
            .client
            .pull_layers(&resolved_image, &oci_cache)
            .await
            .map_err(|e| {
                cuenv_core::Error::tool_resolution(format!(
                    "Failed to pull layers for '{image}': {e}"
                ))
            })?;

        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        extract_from_layers(&layer_paths, path, &final_path).map_err(|e| {
            cuenv_core::Error::tool_resolution(format!(
                "Failed to extract '{path}' from '{image}': {e}"
            ))
        })?;

        ensure_executable(&final_path)?;

        let sha256 = compute_file_sha256(&final_path).await?;
        info!(
            tool = %resolved.name,
            binary = ?final_path,
            %sha256,
            "Fetched OCI tool"
        );

        Ok(FetchedTool {
            name: resolved.name.clone(),
            binary_path: final_path,
            sha256,
        })
    }

    fn is_cached(&self, resolved: &ResolvedTool, options: &ToolOptions) -> bool {
        // Without a manifest round-trip we cannot know the resolved digest at
        // cache-check time. Probe every digest directory we might have written
        // for this tool. This stays consistent with the cache layout in
        // `fetch` and avoids any network call.
        let ToolSource::Oci { path, .. } = &resolved.source else {
            return false;
        };
        let binary_name = Self::binary_name_for(path);

        let root = options.cache_dir().join("oci");
        let Ok(entries) = std::fs::read_dir(&root) else {
            return false;
        };

        entries
            .filter_map(std::result::Result::ok)
            .any(|entry| entry.path().join(binary_name).exists())
    }
}

/// Replace characters that are not safe for directory names (e.g., `:`)
/// while keeping the digest recognizable.
fn sanitize_digest(digest: &str) -> String {
    digest.replace(':', "_")
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
    let _ = path;
    Ok(())
}

/// Compute SHA256 hash of a file (lowercase hex, no algorithm prefix).
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
    use tempfile::TempDir;

    #[test]
    fn test_can_handle_oci_source() {
        let provider = OciToolProvider::new();
        let source = ToolSource::Oci {
            image: "ghcr.io/example/tool:1.0".to_string(),
            path: "/usr/bin/tool".to_string(),
        };
        assert!(provider.can_handle(&source));
    }

    #[test]
    fn test_cannot_handle_url_source() {
        let provider = OciToolProvider::new();
        let source = ToolSource::Url {
            url: "https://example.com/tool".to_string(),
            extract: vec![],
        };
        assert!(!provider.can_handle(&source));
    }

    #[test]
    fn test_provider_name_and_description() {
        let provider = OciToolProvider::new();
        assert_eq!(provider.name(), "oci");
        assert!(!provider.description().is_empty());
    }

    #[test]
    fn test_expand_template_substitutes_version_os_arch() {
        let result = OciToolProvider::expand_template(
            "ghcr.io/owner/{os}/tool:{version}-{arch}",
            "1.2.3",
            &Platform::new(Os::Linux, Arch::Arm64),
        );
        assert_eq!(result, "ghcr.io/owner/linux/tool:1.2.3-aarch64");
    }

    #[test]
    fn test_expand_template_x86_64() {
        let result = OciToolProvider::expand_template(
            "{os}-{arch}",
            "0.1.0",
            &Platform::new(Os::Darwin, Arch::X86_64),
        );
        assert_eq!(result, "darwin-x86_64");
    }

    #[test]
    fn test_to_oci_platform_amd64_mapping() {
        let p = OciToolProvider::to_oci_platform(&Platform::new(Os::Linux, Arch::X86_64));
        // OciPlatform::to_oci_platform converts x86_64 -> amd64.
        assert_eq!(p.to_oci_platform(), "linux/amd64");

        let p = OciToolProvider::to_oci_platform(&Platform::new(Os::Darwin, Arch::Arm64));
        assert_eq!(p.to_oci_platform(), "darwin/arm64");
    }

    #[test]
    fn test_binary_name_for() {
        assert_eq!(
            OciToolProvider::binary_name_for("/usr/local/bin/weaver"),
            "weaver"
        );
        assert_eq!(OciToolProvider::binary_name_for("tool"), "tool");
        assert_eq!(OciToolProvider::binary_name_for("/bin/jq"), "jq");
    }

    #[test]
    fn test_sanitize_digest_replaces_colon() {
        assert_eq!(sanitize_digest("sha256:abc"), "sha256_abc");
        assert_eq!(sanitize_digest("abc"), "abc");
    }

    #[test]
    fn test_binary_cache_dir_uses_sanitized_digest() {
        let temp = TempDir::new().unwrap();
        let options = ToolOptions::new().with_cache_dir(temp.path().to_path_buf());
        let dir = OciToolProvider::binary_cache_dir(&options, "sha256:deadbeef");
        assert_eq!(dir, temp.path().join("oci").join("sha256_deadbeef"));
    }

    #[tokio::test]
    async fn test_resolve_expands_templates_into_source() {
        let provider = OciToolProvider::new();
        let config = serde_json::json!({
            "type": "oci",
            "image": "ghcr.io/owner/{os}-{arch}:{version}",
            "path": "/opt/{os}/tool"
        });
        let platform = Platform::new(Os::Linux, Arch::Arm64);
        let request = ToolResolveRequest {
            tool_name: "demo",
            version: "9.9.9",
            platform: &platform,
            config: &config,
            token: None,
        };

        let resolved = provider.resolve(&request).await.unwrap();
        match resolved.source {
            ToolSource::Oci { image, path } => {
                assert_eq!(image, "ghcr.io/owner/linux-aarch64:9.9.9");
                assert_eq!(path, "/opt/linux/tool");
            }
            _ => panic!("expected OCI source"),
        }
    }

    #[tokio::test]
    async fn test_resolve_requires_image_field() {
        let provider = OciToolProvider::new();
        let config = serde_json::json!({ "type": "oci", "path": "/bin/tool" });
        let platform = Platform::new(Os::Linux, Arch::X86_64);
        let request = ToolResolveRequest {
            tool_name: "demo",
            version: "1.0.0",
            platform: &platform,
            config: &config,
            token: None,
        };
        assert!(provider.resolve(&request).await.is_err());
    }

    #[tokio::test]
    async fn test_resolve_requires_path_field() {
        let provider = OciToolProvider::new();
        let config = serde_json::json!({ "type": "oci", "image": "ghcr.io/x/y:1" });
        let platform = Platform::new(Os::Linux, Arch::X86_64);
        let request = ToolResolveRequest {
            tool_name: "demo",
            version: "1.0.0",
            platform: &platform,
            config: &config,
            token: None,
        };
        assert!(provider.resolve(&request).await.is_err());
    }

    #[test]
    fn test_is_cached_false_when_cache_root_missing() {
        let provider = OciToolProvider::new();
        let temp = TempDir::new().unwrap();
        let options = ToolOptions::new().with_cache_dir(temp.path().join("does-not-exist"));
        let resolved = ResolvedTool {
            name: "tool".to_string(),
            version: "1.0".to_string(),
            platform: Platform::new(Os::Linux, Arch::Arm64),
            source: ToolSource::Oci {
                image: "ghcr.io/x/y:1".to_string(),
                path: "/usr/bin/tool".to_string(),
            },
        };
        assert!(!provider.is_cached(&resolved, &options));
    }

    #[test]
    fn test_is_cached_true_when_binary_present() {
        let provider = OciToolProvider::new();
        let temp = TempDir::new().unwrap();
        let options = ToolOptions::new().with_cache_dir(temp.path().to_path_buf());

        let bin_dir = temp.path().join("oci").join("sha256_abc");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(bin_dir.join("tool"), b"#!/bin/sh\n").unwrap();

        let resolved = ResolvedTool {
            name: "tool".to_string(),
            version: "1.0".to_string(),
            platform: Platform::new(Os::Linux, Arch::Arm64),
            source: ToolSource::Oci {
                image: "ghcr.io/x/y:1".to_string(),
                path: "/usr/local/bin/tool".to_string(),
            },
        };
        assert!(provider.is_cached(&resolved, &options));
    }

    #[test]
    fn test_is_cached_false_for_non_oci_source() {
        let provider = OciToolProvider::new();
        let temp = TempDir::new().unwrap();
        let options = ToolOptions::new().with_cache_dir(temp.path().to_path_buf());
        let resolved = ResolvedTool {
            name: "tool".to_string(),
            version: "1.0".to_string(),
            platform: Platform::new(Os::Linux, Arch::Arm64),
            source: ToolSource::Url {
                url: "https://example.com/tool".to_string(),
                extract: vec![],
            },
        };
        assert!(!provider.is_cached(&resolved, &options));
    }
}
