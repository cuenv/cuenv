//! Homebrew bottle tool provider for cuenv.
//!
//! Fetches development tools from Homebrew bottles hosted at ghcr.io/homebrew.
//! Supports automatic dependency resolution and binary relocation.

use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_core::tools::{
    FetchedTool, Platform, ResolvedTool, ToolOptions, ToolProvider, ToolSource,
};
use cuenv_tools_oci::{
    OciCache, OciClient, extract_homebrew_bottle, fetch_formula, relocate_homebrew_bottle,
    resolve_with_deps, to_homebrew_platform,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::io::AsyncReadExt;
use tracing::{debug, info};

/// Sanitize a formula name for use in an OCI image reference.
///
/// Homebrew versioned formulas like "python@3.14" are published with the `@` replaced
/// by `/` in the OCI path, e.g., `ghcr.io/homebrew/core/python/3.14:version`.
fn sanitize_formula_for_oci(name: &str) -> String {
    name.replace('@', "/")
}

/// Tool provider for Homebrew bottles.
///
/// Fetches pre-built binaries from ghcr.io/homebrew/core using OCI registry protocol.
/// Handles dependency resolution and binary relocation for dynamically linked tools.
pub struct HomebrewToolProvider {
    client: OciClient,
}

impl Default for HomebrewToolProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl HomebrewToolProvider {
    /// Create a new Homebrew tool provider.
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: OciClient::new(),
        }
    }

    /// Get the cache directory for a formula.
    fn formula_cache_dir(&self, options: &ToolOptions, name: &str, version: &str) -> PathBuf {
        options
            .cache_dir()
            .join("homebrew")
            .join(name)
            .join(version)
    }

    /// Get the homebrew root cache directory.
    fn homebrew_cache_dir(&self, options: &ToolOptions) -> PathBuf {
        options.cache_dir().join("homebrew")
    }
}

#[async_trait]
impl ToolProvider for HomebrewToolProvider {
    fn name(&self) -> &'static str {
        "homebrew"
    }

    fn description(&self) -> &'static str {
        "Fetch tools from Homebrew bottles (ghcr.io/homebrew)"
    }

    fn can_handle(&self, source: &ToolSource) -> bool {
        matches!(source, ToolSource::Homebrew { .. })
    }

    async fn resolve(
        &self,
        tool_name: &str,
        version: &str,
        platform: &Platform,
        config: &serde_json::Value,
    ) -> Result<ResolvedTool> {
        // Extract formula name (may differ from tool name)
        let formula = config
            .get("formula")
            .and_then(|v| v.as_str())
            .unwrap_or(tool_name);

        info!(%tool_name, %formula, %version, %platform, "Resolving Homebrew formula");

        // Map platform to Homebrew platform string
        let platform_str = format!("{}", platform);
        let homebrew_platform = to_homebrew_platform(&platform_str).ok_or_else(|| {
            cuenv_core::Error::platform(format!(
                "Platform '{}' not supported by Homebrew",
                platform
            ))
        })?;

        // Verify formula exists and has a bottle for this platform
        let formula_info = fetch_formula(formula).await.map_err(|e| {
            cuenv_core::Error::tool_resolution(format!(
                "Failed to fetch formula '{}': {}",
                formula, e
            ))
        })?;

        // Homebrew only publishes bottles for the current stable version.
        // Always use the stable version - historical versions are not available.
        let actual_version = formula_info.versions.stable.clone();

        if version != "latest" && version != actual_version {
            debug!(
                requested = %version,
                stable = %actual_version,
                "Requested version differs from stable; using stable version"
            );
        }

        // Verify bottle exists for platform
        if formula_info.get_bottle(&homebrew_platform).is_none() {
            return Err(cuenv_core::Error::tool_resolution(format!(
                "No Homebrew bottle for '{}' on platform '{}' (available: {:?})",
                formula,
                homebrew_platform,
                formula_info.available_platforms()
            )));
        }

        // Build OCI image reference (sanitize formula name for versioned formulas like python@3.14)
        let sanitized_formula = sanitize_formula_for_oci(formula);
        let image_ref = format!(
            "ghcr.io/homebrew/core/{}:{}",
            sanitized_formula, actual_version
        );

        debug!(%image_ref, %homebrew_platform, "Resolved to OCI image");

        Ok(ResolvedTool {
            name: tool_name.to_string(),
            version: actual_version,
            platform: platform.clone(),
            source: ToolSource::Homebrew {
                formula: formula.to_string(),
                image_ref,
            },
        })
    }

    async fn fetch(&self, resolved: &ResolvedTool, options: &ToolOptions) -> Result<FetchedTool> {
        let ToolSource::Homebrew { formula, .. } = &resolved.source else {
            return Err(cuenv_core::Error::tool_resolution(
                "HomebrewToolProvider received non-Homebrew source",
            ));
        };

        info!(
            tool = %resolved.name,
            formula = %formula,
            version = %resolved.version,
            "Fetching Homebrew bottle"
        );

        // Check cache first
        let cache_dir = self.formula_cache_dir(options, formula, &resolved.version);
        let binary_path = cache_dir.join("bin").join(&resolved.name);

        if binary_path.exists() && !options.force_refetch {
            debug!(?binary_path, "Tool already cached");
            let sha256 = compute_file_sha256(&binary_path).await?;
            return Ok(FetchedTool {
                name: resolved.name.clone(),
                binary_path,
                sha256,
            });
        }

        // Resolve formula with dependencies
        let formulas = resolve_with_deps(formula).await.map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to resolve dependencies: {}", e))
        })?;

        // On Linux, add glibc as an implicit dependency for all Homebrew bottles.
        // Homebrew bottles are built against Homebrew's glibc and require its ld.so.
        #[cfg(target_os = "linux")]
        let formulas = {
            let mut formulas = formulas;
            if !formulas.iter().any(|f| f.name == "glibc") {
                let glibc = fetch_formula("glibc").await.map_err(|e| {
                    cuenv_core::Error::tool_resolution(format!("Failed to fetch glibc: {}", e))
                })?;
                // Insert at beginning so glibc is fetched first
                formulas.insert(0, glibc);
            }
            formulas
        };

        // Build dependency version map for relocation
        let mut dep_versions: HashMap<String, String> = HashMap::new();
        for f in &formulas {
            dep_versions.insert(f.name.clone(), f.versions.stable.clone());
        }

        // Set up OCI cache
        let oci_cache = OciCache::new(options.cache_dir().join("oci"));

        // Fetch all formulas (dependencies first)
        let homebrew_cache = self.homebrew_cache_dir(options);
        let platform_str = format!("{}", resolved.platform);
        let oci_platform = cuenv_tools_oci::Platform::parse(&platform_str).ok_or_else(|| {
            cuenv_core::Error::platform(format!("Invalid platform: {}", platform_str))
        })?;

        // Get homebrew platform for bottle availability checks
        let homebrew_platform = to_homebrew_platform(&platform_str).ok_or_else(|| {
            cuenv_core::Error::platform(format!(
                "Platform '{}' not supported by Homebrew",
                platform_str
            ))
        })?;

        for f in &formulas {
            let formula_dir = homebrew_cache.join(&f.name).join(&f.versions.stable);

            // Skip if already extracted
            if formula_dir.join("bin").exists() || formula_dir.join("lib").exists() {
                debug!(name = %f.name, "Formula already extracted");
                continue;
            }

            // Skip formulas without bottles for this platform (e.g., ca-certificates on some platforms)
            if f.get_bottle(&homebrew_platform).is_none() {
                debug!(
                    name = %f.name,
                    %homebrew_platform,
                    "No bottle for platform, skipping dependency"
                );
                continue;
            }

            // Pull the bottle (sanitize formula name for versioned formulas like python@3.14)
            let sanitized_name = sanitize_formula_for_oci(&f.name);
            let f_image_ref = format!(
                "ghcr.io/homebrew/core/{}:{}",
                sanitized_name, f.versions.stable
            );
            debug!(image = %f_image_ref, "Pulling bottle");

            let resolved_image = self
                .client
                .resolve_digest(&f_image_ref, &oci_platform)
                .await
                .map_err(|e| {
                    cuenv_core::Error::tool_resolution(format!(
                        "Failed to resolve image '{}': {}",
                        f_image_ref, e
                    ))
                })?;

            // Pull layers
            let layer_paths = self
                .client
                .pull_layers(&resolved_image, &oci_cache)
                .await
                .map_err(|e| {
                    cuenv_core::Error::tool_resolution(format!(
                        "Failed to pull layers for '{}': {}",
                        f.name, e
                    ))
                })?;

            // Extract bottle (first layer is the bottle)
            if let Some(bottle_path) = layer_paths.first() {
                extract_homebrew_bottle(bottle_path, &formula_dir).map_err(|e| {
                    cuenv_core::Error::tool_resolution(format!(
                        "Failed to extract bottle '{}': {}",
                        f.name, e
                    ))
                })?;
            }

            // Relocate binaries (macOS only)
            relocate_homebrew_bottle(
                &formula_dir,
                &homebrew_cache,
                &f.name,
                &f.versions.stable,
                &dep_versions,
            )
            .map_err(|e| {
                cuenv_core::Error::tool_resolution(format!(
                    "Failed to relocate bottle '{}': {}",
                    f.name, e
                ))
            })?;
        }

        // Verify the tool binary exists
        let mut actual_binary_path = binary_path.clone();

        if !actual_binary_path.exists() {
            // Binary might have a different name than the tool
            // Search for any executable in bin/
            let bin_dir = cache_dir.join("bin");
            let mut found = false;

            if bin_dir.exists() {
                for entry in std::fs::read_dir(&bin_dir)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.is_file() {
                        // Check if it's executable (on Unix)
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            if let Ok(metadata) = path.metadata() {
                                if metadata.permissions().mode() & 0o111 != 0 {
                                    debug!(
                                        expected = %resolved.name,
                                        found = ?path.file_name(),
                                        "Using alternative binary name"
                                    );
                                    actual_binary_path = path;
                                    found = true;
                                    break;
                                }
                            }
                        }
                        #[cfg(not(unix))]
                        {
                            actual_binary_path = path;
                            found = true;
                            break;
                        }
                    }
                }
            }

            if !found {
                return Err(cuenv_core::Error::tool_resolution(format!(
                    "Binary '{}' not found in extracted bottle (checked: {:?})",
                    resolved.name, bin_dir
                )));
            }
        }

        let sha256 = compute_file_sha256(&actual_binary_path).await?;

        info!(
            tool = %resolved.name,
            binary = ?actual_binary_path,
            sha256 = %sha256,
            "Fetched Homebrew tool"
        );

        Ok(FetchedTool {
            name: resolved.name.clone(),
            binary_path: actual_binary_path,
            sha256,
        })
    }

    fn is_cached(&self, resolved: &ResolvedTool, options: &ToolOptions) -> bool {
        let ToolSource::Homebrew { formula, .. } = &resolved.source else {
            return false;
        };

        let cache_dir = self.formula_cache_dir(options, formula, &resolved.version);
        let binary_path = cache_dir.join("bin").join(&resolved.name);

        // Check exact name first
        if binary_path.exists() {
            return true;
        }

        // Check if any binary exists in bin/ (alternative binary names)
        let bin_dir = cache_dir.join("bin");
        if bin_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&bin_dir) {
                for entry in entries.flatten() {
                    if entry.path().is_file() {
                        return true;
                    }
                }
            }
        }

        false
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
        let provider = HomebrewToolProvider::new();
        assert_eq!(provider.name(), "homebrew");
    }

    #[test]
    fn test_can_handle() {
        let provider = HomebrewToolProvider::new();

        let homebrew_source = ToolSource::Homebrew {
            formula: "jq".into(),
            image_ref: "ghcr.io/homebrew/core/jq:1.7.1".into(),
        };
        assert!(provider.can_handle(&homebrew_source));

        let github_source = ToolSource::GitHub {
            repo: "org/repo".into(),
            tag: "v1".into(),
            asset: "file.zip".into(),
            path: None,
        };
        assert!(!provider.can_handle(&github_source));
    }
}
