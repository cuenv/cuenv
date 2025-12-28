//! Nix flake tool provider for cuenv.
//!
//! Fetches development tools from Nix flakes using `nix build`.
//! Supports named flake references for pinning different nixpkgs versions.

use async_trait::async_trait;
use cuenv_core::tools::{
    FetchedTool, Platform, ResolvedTool, ToolOptions, ToolProvider, ToolSource,
};
use cuenv_core::Result;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tracing::{debug, info};

/// Tool provider for Nix flakes.
///
/// Builds and caches tools from Nix flakes. Supports named flake references
/// that are resolved from the runtime configuration.
pub struct NixToolProvider {
    /// Named flake references (e.g., "stable" -> "github:NixOS/nixpkgs/nixos-24.11").
    flakes: std::collections::HashMap<String, String>,
}

impl Default for NixToolProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl NixToolProvider {
    /// Create a new Nix tool provider.
    #[must_use]
    pub fn new() -> Self {
        Self {
            flakes: std::collections::HashMap::new(),
        }
    }

    /// Create a Nix tool provider with named flake references.
    #[must_use]
    pub fn with_flakes(flakes: std::collections::HashMap<String, String>) -> Self {
        Self { flakes }
    }

    /// Get the cache directory for a tool.
    fn tool_cache_dir(&self, options: &ToolOptions, name: &str, version: &str) -> PathBuf {
        options.cache_dir().join("nix").join(name).join(version)
    }

    /// Resolve a flake name to its full reference.
    fn resolve_flake(&self, name: &str) -> Option<&str> {
        self.flakes.get(name).map(String::as_str)
    }

    /// Check if nix is available.
    async fn check_nix_available() -> bool {
        Command::new("nix")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

#[async_trait]
impl ToolProvider for NixToolProvider {
    fn name(&self) -> &'static str {
        "nix"
    }

    fn description(&self) -> &'static str {
        "Build tools from Nix flakes"
    }

    fn can_handle(&self, source: &ToolSource) -> bool {
        matches!(source, ToolSource::Nix { .. })
    }

    async fn resolve(
        &self,
        tool_name: &str,
        version: &str,
        platform: &Platform,
        config: &serde_json::Value,
    ) -> Result<ResolvedTool> {
        let flake_name = config
            .get("flake")
            .and_then(|v| v.as_str())
            .ok_or_else(|| cuenv_core::Error::tool_resolution("Missing 'flake' in config"))?;

        let package = config
            .get("package")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                cuenv_core::Error::tool_resolution("Missing 'package' in config")
            })?;

        let output = config.get("output").and_then(|v| v.as_str());

        info!(%tool_name, %flake_name, %package, %version, %platform, "Resolving Nix package");

        // Check if nix is available
        if !Self::check_nix_available().await {
            return Err(cuenv_core::Error::tool_resolution(
                "Nix is not installed or not in PATH".to_string(),
            ));
        }

        // Resolve flake name to full reference
        let flake_ref = self.resolve_flake(flake_name).ok_or_else(|| {
            cuenv_core::Error::tool_resolution(format!(
                "Unknown flake '{}'. Add it to runtime.flakes in your config.",
                flake_name
            ))
        })?;

        debug!(%flake_name, %flake_ref, %package, "Resolved flake reference");

        Ok(ResolvedTool {
            name: tool_name.to_string(),
            version: version.to_string(),
            platform: platform.clone(),
            source: ToolSource::Nix {
                flake: flake_ref.to_string(),
                package: package.to_string(),
                output: output.map(String::from),
            },
        })
    }

    async fn fetch(&self, resolved: &ResolvedTool, options: &ToolOptions) -> Result<FetchedTool> {
        let ToolSource::Nix { flake, package, output } = &resolved.source else {
            return Err(cuenv_core::Error::tool_resolution(
                "NixToolProvider received non-Nix source".to_string(),
            ));
        };

        info!(
            tool = %resolved.name,
            %flake,
            %package,
            "Building Nix package"
        );

        // Check cache
        let cache_dir = self.tool_cache_dir(options, &resolved.name, &resolved.version);
        let binary_path = cache_dir.join(&resolved.name);

        if binary_path.exists() && !options.force_refetch {
            debug!(?binary_path, "Tool already cached");
            let sha256 = compute_file_sha256(&binary_path).await?;
            return Ok(FetchedTool {
                name: resolved.name.clone(),
                binary_path,
                sha256,
            });
        }

        // Build the package
        let flake_attr = format!("{}#{}", flake, package);
        debug!(%flake_attr, "Running nix build");

        let output_result = Command::new("nix")
            .args(["build", "--no-link", "--print-out-paths", &flake_attr])
            .output()
            .await
            .map_err(|e| cuenv_core::Error::tool_resolution(format!("Failed to run nix build: {}", e)))?;

        if !output_result.status.success() {
            let stderr = String::from_utf8_lossy(&output_result.stderr);
            return Err(cuenv_core::Error::tool_resolution(format!(
                "nix build failed: {}",
                stderr
            )));
        }

        let store_path = String::from_utf8_lossy(&output_result.stdout)
            .trim()
            .to_string();

        debug!(%store_path, "Built Nix package");

        // Find the binary
        let bin_path = if let Some(out) = output {
            PathBuf::from(&store_path).join(out.trim_start_matches('/'))
        } else {
            // Try common locations
            let candidates = [
                PathBuf::from(&store_path).join("bin").join(&resolved.name),
                PathBuf::from(&store_path).join("bin").join(package),
            ];

            candidates
                .into_iter()
                .find(|p| p.exists())
                .ok_or_else(|| {
                    cuenv_core::Error::tool_resolution(
                        "Binary not found in Nix output. Try specifying 'output' in config."
                    )
                })?
        };

        if !bin_path.exists() {
            return Err(cuenv_core::Error::tool_resolution(format!(
                "Binary not found at '{}'",
                bin_path.display()
            )));
        }

        // Copy to cache directory
        std::fs::create_dir_all(&cache_dir)?;
        std::fs::copy(&bin_path, &binary_path)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&binary_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&binary_path, perms)?;
        }

        let sha256 = compute_file_sha256(&binary_path).await?;

        info!(
            tool = %resolved.name,
            binary = ?binary_path,
            %sha256,
            "Fetched Nix tool"
        );

        Ok(FetchedTool {
            name: resolved.name.clone(),
            binary_path,
            sha256,
        })
    }

    fn is_cached(&self, resolved: &ResolvedTool, options: &ToolOptions) -> bool {
        let cache_dir = self.tool_cache_dir(options, &resolved.name, &resolved.version);
        let binary_path = cache_dir.join(&resolved.name);
        binary_path.exists()
    }

    async fn check_prerequisites(&self) -> Result<()> {
        let output = Command::new("nix")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        match output {
            Ok(status) if status.success() => Ok(()),
            Ok(_) => Err(cuenv_core::Error::tool_resolution_with_help(
                "Nix command failed",
                "Install Nix: https://nixos.org/download.html",
            )),
            Err(_) => Err(cuenv_core::Error::tool_resolution_with_help(
                "Nix is not installed",
                "Install Nix: https://nixos.org/download.html",
            )),
        }
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
        let provider = NixToolProvider::new();
        assert_eq!(provider.name(), "nix");
    }

    #[test]
    fn test_resolve_flake() {
        let mut flakes = std::collections::HashMap::new();
        flakes.insert("stable".to_string(), "github:NixOS/nixpkgs/nixos-24.11".to_string());
        flakes.insert("unstable".to_string(), "github:NixOS/nixpkgs/nixos-unstable".to_string());

        let provider = NixToolProvider::with_flakes(flakes);

        assert_eq!(
            provider.resolve_flake("stable"),
            Some("github:NixOS/nixpkgs/nixos-24.11")
        );
        assert_eq!(
            provider.resolve_flake("unstable"),
            Some("github:NixOS/nixpkgs/nixos-unstable")
        );
        assert_eq!(provider.resolve_flake("unknown"), None);
    }

    #[test]
    fn test_can_handle() {
        let provider = NixToolProvider::new();

        let nix_source = ToolSource::Nix {
            flake: "github:NixOS/nixpkgs",
            package: "jq",
            output: None,
        };
        assert!(provider.can_handle(&nix_source));

        let homebrew_source = ToolSource::Homebrew {
            formula: "jq",
            image_ref: "ghcr.io/homebrew/core/jq:1.7.1",
        };
        assert!(!provider.can_handle(&homebrew_source));
    }
}
