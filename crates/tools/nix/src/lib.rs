//! Nix flake tool provider for cuenv.
//!
//! Provides Nix-based tool resolution using flakes. Tools are installed into
//! per-project Nix profiles (managed by the `profile` module) rather than
//! copying individual binaries, ensuring full Nix closures are available.

pub mod commands;
pub mod profile;

use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_core::tools::{
    FetchedTool, Platform, ResolvedTool, ToolOptions, ToolProvider, ToolSource,
};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;
use tracing::{debug, info};

/// Tool provider for Nix flakes.
///
/// Resolves tools from Nix flakes. Actual installation into per-project
/// Nix profiles is handled by the `profile` module during sync.
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
            .ok_or_else(|| cuenv_core::Error::tool_resolution("Missing 'package' in config"))?;

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

    async fn fetch(&self, resolved: &ResolvedTool, _options: &ToolOptions) -> Result<FetchedTool> {
        let ToolSource::Nix { flake, package, .. } = &resolved.source else {
            return Err(cuenv_core::Error::tool_resolution(
                "NixToolProvider received non-Nix source".to_string(),
            ));
        };

        // For Nix, we don't fetch/cache binaries - profiles handle this.
        // Just validate the package exists by checking if it can be evaluated.
        let flake_ref = format!("{flake}#{package}");
        debug!(%flake_ref, "Validating Nix package exists");

        // Use nix eval to quickly check if the package exists (no build required)
        let output = Command::new("nix")
            .args(["eval", "--raw", &format!("{flake_ref}.name")])
            .output()
            .await
            .map_err(|e| cuenv_core::Error::tool_resolution(format!("Failed to run nix: {e}")))?;

        if !output.status.success() {
            // Package might not have a .name attribute, try just evaluating the derivation
            let output2 = Command::new("nix")
                .args(["eval", "--raw", &format!("{flake_ref}.type")])
                .output()
                .await
                .map_err(|e| {
                    cuenv_core::Error::tool_resolution(format!("Failed to run nix: {e}"))
                })?;

            if !output2.status.success() {
                return Err(cuenv_core::Error::tool_resolution(format!(
                    "Nix package {flake_ref} not found or cannot be evaluated"
                )));
            }
        }

        info!(tool = %resolved.name, %flake_ref, "Validated Nix package");

        // Return placeholder - actual binary comes from profile
        Ok(FetchedTool {
            name: resolved.name.clone(),
            binary_path: PathBuf::new(), // Profile manages the actual path
            sha256: "nix-profile-managed".to_string(),
        })
    }

    fn is_cached(&self, _resolved: &ResolvedTool, _options: &ToolOptions) -> bool {
        // Nix tools are always "cached" via the Nix store.
        // Profile installation is handled by profile::ensure_profile() during sync.
        true
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

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::tools::{Arch, Os};

    #[test]
    fn test_provider_name() {
        let provider = NixToolProvider::new();
        assert_eq!(provider.name(), "nix");
    }

    #[test]
    fn test_provider_description() {
        let provider = NixToolProvider::new();
        assert_eq!(provider.description(), "Build tools from Nix flakes");
    }

    #[test]
    fn test_provider_default() {
        let provider = NixToolProvider::default();
        assert_eq!(provider.name(), "nix");
        // Default provider has no flakes registered
        assert!(provider.resolve_flake("stable").is_none());
    }

    #[test]
    fn test_provider_new_empty_flakes() {
        let provider = NixToolProvider::new();
        assert!(provider.resolve_flake("anything").is_none());
    }

    #[test]
    fn test_resolve_flake() {
        let mut flakes = std::collections::HashMap::new();
        flakes.insert(
            "stable".to_string(),
            "github:NixOS/nixpkgs/nixos-24.11".to_string(),
        );
        flakes.insert(
            "unstable".to_string(),
            "github:NixOS/nixpkgs/nixos-unstable".to_string(),
        );

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
    fn test_resolve_flake_with_custom_flakes() {
        let mut flakes = std::collections::HashMap::new();
        flakes.insert(
            "my-tools".to_string(),
            "github:owner/my-tools-flake".to_string(),
        );
        flakes.insert("local".to_string(), "path:./flake".to_string());

        let provider = NixToolProvider::with_flakes(flakes);

        assert_eq!(
            provider.resolve_flake("my-tools"),
            Some("github:owner/my-tools-flake")
        );
        assert_eq!(provider.resolve_flake("local"), Some("path:./flake"));
    }

    #[test]
    fn test_can_handle() {
        let provider = NixToolProvider::new();

        let nix_source = ToolSource::Nix {
            flake: "github:NixOS/nixpkgs".into(),
            package: "jq".into(),
            output: None,
        };
        assert!(provider.can_handle(&nix_source));

        let github_source = ToolSource::GitHub {
            repo: "jqlang/jq".into(),
            tag: "v1.7.1".into(),
            asset: "jq-linux-amd64".into(),
            path: None,
        };
        assert!(!provider.can_handle(&github_source));
    }

    #[test]
    fn test_can_handle_nix_with_output() {
        let provider = NixToolProvider::new();

        let nix_source = ToolSource::Nix {
            flake: "github:NixOS/nixpkgs".into(),
            package: "hello".into(),
            output: Some("out".to_string()),
        };
        assert!(provider.can_handle(&nix_source));
    }

    #[test]
    fn test_can_handle_rustup_source() {
        let provider = NixToolProvider::new();

        let rustup_source = ToolSource::Rustup {
            toolchain: "stable".into(),
            profile: None,
            components: vec![],
            targets: vec![],
        };
        assert!(!provider.can_handle(&rustup_source));
    }

    #[test]
    fn test_can_handle_oci_source() {
        let provider = NixToolProvider::new();

        let oci_source = ToolSource::Oci {
            image: "alpine:latest".into(),
            path: "alpine".into(),
        };
        assert!(!provider.can_handle(&oci_source));
    }

    #[test]
    fn test_is_cached_always_true() {
        let provider = NixToolProvider::new();
        let options = ToolOptions::new();

        // Create a resolved tool with Nix source
        let resolved = ResolvedTool {
            name: "jq".to_string(),
            version: "1.7.1".to_string(),
            platform: Platform::new(Os::Darwin, Arch::Arm64),
            source: ToolSource::Nix {
                flake: "github:NixOS/nixpkgs".to_string(),
                package: "jq".to_string(),
                output: None,
            },
        };

        // Nix tools are always considered cached
        assert!(provider.is_cached(&resolved, &options));
    }

    #[test]
    fn test_is_cached_non_nix_source() {
        let provider = NixToolProvider::new();
        let options = ToolOptions::new();

        // Create a resolved tool with GitHub source
        let resolved = ResolvedTool {
            name: "mytool".to_string(),
            version: "1.0.0".to_string(),
            platform: Platform::new(Os::Linux, Arch::X86_64),
            source: ToolSource::GitHub {
                repo: "owner/repo".to_string(),
                tag: "v1.0.0".to_string(),
                asset: "file.zip".to_string(),
                path: None,
            },
        };

        // Even non-Nix sources return true since we just return true unconditionally
        assert!(provider.is_cached(&resolved, &options));
    }

    #[test]
    fn test_with_flakes_empty() {
        let provider = NixToolProvider::with_flakes(std::collections::HashMap::new());
        assert!(provider.resolve_flake("any").is_none());
    }

    #[test]
    fn test_with_flakes_single() {
        let mut flakes = std::collections::HashMap::new();
        flakes.insert("nixpkgs".to_string(), "github:NixOS/nixpkgs".to_string());

        let provider = NixToolProvider::with_flakes(flakes);
        assert_eq!(
            provider.resolve_flake("nixpkgs"),
            Some("github:NixOS/nixpkgs")
        );
    }

    #[test]
    fn test_with_flakes_overwrite() {
        let mut flakes = std::collections::HashMap::new();
        flakes.insert("stable".to_string(), "first-value".to_string());
        flakes.insert("stable".to_string(), "second-value".to_string());

        let provider = NixToolProvider::with_flakes(flakes);
        // HashMap uses last inserted value
        assert_eq!(provider.resolve_flake("stable"), Some("second-value"));
    }
}
