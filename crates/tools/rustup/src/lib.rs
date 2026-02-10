//! Rustup tool provider for cuenv.
//!
//! Manages Rust toolchains via rustup. Supports:
//! - Specific version toolchains (e.g., "1.83.0", "stable", "nightly")
//! - Installation profiles (minimal, default, complete)
//! - Additional components (clippy, rustfmt, rust-src, etc.)
//! - Cross-compilation targets

use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_core::tools::{
    Arch, FetchedTool, Os, Platform, ResolvedTool, ToolOptions, ToolProvider, ToolResolveRequest,
    ToolSource,
};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tokio::process::Command;
use tracing::{debug, info};

/// Tool provider for rustup-managed Rust toolchains.
///
/// Uses the system's rustup installation to manage Rust toolchains,
/// components, and cross-compilation targets.
pub struct RustupToolProvider;

impl Default for RustupToolProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl RustupToolProvider {
    /// Create a new rustup tool provider.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Get the rustup home directory.
    fn rustup_home() -> PathBuf {
        std::env::var("RUSTUP_HOME").map_or_else(
            |_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".rustup")
            },
            PathBuf::from,
        )
    }

    /// Get the host triple for the current platform.
    fn host_triple(platform: &Platform) -> String {
        let arch = match platform.arch {
            Arch::Arm64 => "aarch64",
            Arch::X86_64 => "x86_64",
        };
        let os = match platform.os {
            Os::Darwin => "apple-darwin",
            Os::Linux => "unknown-linux-gnu",
        };
        format!("{arch}-{os}")
    }

    /// Get the toolchain directory path.
    fn toolchain_path(toolchain: &str, platform: &Platform) -> PathBuf {
        let host_triple = Self::host_triple(platform);
        // Rustup stores toolchains as either:
        // - "{version}-{triple}" for versioned toolchains (e.g., "1.83.0-x86_64-apple-darwin")
        // - "{channel}-{triple}" for channel toolchains (e.g., "stable-x86_64-apple-darwin")
        let toolchain_name = format!("{toolchain}-{host_triple}");
        Self::rustup_home().join("toolchains").join(toolchain_name)
    }

    /// Check if a toolchain is installed.
    fn is_toolchain_installed(toolchain: &str, platform: &Platform) -> bool {
        let path = Self::toolchain_path(toolchain, platform);
        path.join("bin").join("rustc").exists()
    }

    /// Install a toolchain with the given configuration.
    async fn install_toolchain(
        &self,
        toolchain: &str,
        profile: Option<&str>,
        components: &[String],
        targets: &[String],
    ) -> Result<()> {
        let mut cmd = Command::new("rustup");
        cmd.arg("toolchain").arg("install").arg(toolchain);

        // Add profile if specified
        if let Some(p) = profile {
            cmd.arg("--profile").arg(p);
        }

        // Add components
        for component in components {
            cmd.arg("-c").arg(component);
        }

        // Add targets
        for target in targets {
            cmd.arg("-t").arg(target);
        }

        info!(
            %toolchain,
            ?profile,
            ?components,
            ?targets,
            "Installing Rust toolchain"
        );

        let output = cmd.output().await.map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to run rustup: {e}"))
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(cuenv_core::Error::tool_resolution(format!(
                "rustup toolchain install failed: {stderr}"
            )));
        }

        debug!(%toolchain, "Toolchain installed successfully");
        Ok(())
    }

    /// Compute a digest for the toolchain configuration.
    fn compute_digest(
        toolchain: &str,
        profile: Option<&str>,
        components: &[String],
        targets: &[String],
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(toolchain.as_bytes());
        hasher.update(b"|");
        hasher.update(profile.unwrap_or("default").as_bytes());
        hasher.update(b"|");
        for c in components {
            hasher.update(c.as_bytes());
            hasher.update(b",");
        }
        hasher.update(b"|");
        for t in targets {
            hasher.update(t.as_bytes());
            hasher.update(b",");
        }
        format!("sha256:{:x}", hasher.finalize())
    }
}

#[async_trait]
impl ToolProvider for RustupToolProvider {
    fn name(&self) -> &'static str {
        "rustup"
    }

    fn description(&self) -> &'static str {
        "Manage Rust toolchains via rustup"
    }

    fn can_handle(&self, source: &ToolSource) -> bool {
        matches!(source, ToolSource::Rustup { .. })
    }

    async fn check_prerequisites(&self) -> Result<()> {
        // Check if rustup is available
        let output = Command::new("rustup")
            .arg("--version")
            .output()
            .await
            .map_err(|e| {
                cuenv_core::Error::tool_resolution(format!(
                    "rustup not found. Please install rustup: https://rustup.rs\nError: {e}"
                ))
            })?;

        if !output.status.success() {
            return Err(cuenv_core::Error::tool_resolution(
                "rustup --version failed. Is rustup properly installed?".to_string(),
            ));
        }

        debug!("rustup is available");
        Ok(())
    }

    async fn resolve(&self, request: &ToolResolveRequest<'_>) -> Result<ResolvedTool> {
        let tool_name = request.tool_name;
        let version = request.version;
        let platform = request.platform;
        let config = request.config;

        let toolchain = config
            .get("toolchain")
            .and_then(|v| v.as_str())
            .unwrap_or(version);

        let profile = config
            .get("profile")
            .and_then(|v| v.as_str())
            .map(String::from);

        let components: Vec<String> = config
            .get("components")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let targets: Vec<String> = config
            .get("targets")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        info!(
            %tool_name,
            %toolchain,
            ?profile,
            ?components,
            ?targets,
            %platform,
            "Resolving rustup toolchain"
        );

        Ok(ResolvedTool {
            name: tool_name.to_string(),
            version: version.to_string(),
            platform: platform.clone(),
            source: ToolSource::Rustup {
                toolchain: toolchain.to_string(),
                profile,
                components,
                targets,
            },
        })
    }

    async fn fetch(&self, resolved: &ResolvedTool, _options: &ToolOptions) -> Result<FetchedTool> {
        let ToolSource::Rustup {
            toolchain,
            profile,
            components,
            targets,
        } = &resolved.source
        else {
            return Err(cuenv_core::Error::tool_resolution(
                "RustupToolProvider received non-Rustup source".to_string(),
            ));
        };

        info!(
            tool = %resolved.name,
            %toolchain,
            "Fetching rustup toolchain"
        );

        // Install the toolchain (idempotent - safe to re-run)
        self.install_toolchain(toolchain, profile.as_deref(), components, targets)
            .await?;

        // Get the binary path
        let toolchain_dir = Self::toolchain_path(toolchain, &resolved.platform);
        let bin_dir = toolchain_dir.join("bin");

        // For rust toolchain, the "binary" is actually the bin directory
        // We'll point to cargo as the main binary since that's typically what's used
        let binary_path = bin_dir.join("cargo");

        if !binary_path.exists() {
            return Err(cuenv_core::Error::tool_resolution(format!(
                "Toolchain installed but cargo not found at {}",
                binary_path.display()
            )));
        }

        let sha256 = Self::compute_digest(toolchain, profile.as_deref(), components, targets);

        info!(
            tool = %resolved.name,
            binary = ?bin_dir,
            %sha256,
            "Fetched rustup toolchain"
        );

        Ok(FetchedTool {
            name: resolved.name.clone(),
            binary_path: bin_dir,
            sha256,
        })
    }

    fn is_cached(&self, resolved: &ResolvedTool, _options: &ToolOptions) -> bool {
        let ToolSource::Rustup { toolchain, .. } = &resolved.source else {
            return false;
        };

        let installed = Self::is_toolchain_installed(toolchain, &resolved.platform);
        if installed {
            debug!(%toolchain, "Toolchain already installed");
        }
        installed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_name() {
        let provider = RustupToolProvider::new();
        assert_eq!(provider.name(), "rustup");
    }

    #[test]
    fn test_provider_description() {
        let provider = RustupToolProvider::new();
        assert_eq!(provider.description(), "Manage Rust toolchains via rustup");
    }

    #[test]
    fn test_provider_default() {
        let provider = RustupToolProvider;
        assert_eq!(provider.name(), "rustup");
    }

    #[test]
    fn test_host_triple() {
        let platform = Platform::new(Os::Darwin, Arch::Arm64);
        assert_eq!(
            RustupToolProvider::host_triple(&platform),
            "aarch64-apple-darwin"
        );

        let platform = Platform::new(Os::Linux, Arch::X86_64);
        assert_eq!(
            RustupToolProvider::host_triple(&platform),
            "x86_64-unknown-linux-gnu"
        );
    }

    #[test]
    fn test_host_triple_all_combos() {
        // Darwin + Arm64
        let platform = Platform::new(Os::Darwin, Arch::Arm64);
        assert_eq!(
            RustupToolProvider::host_triple(&platform),
            "aarch64-apple-darwin"
        );

        // Darwin + X86_64
        let platform = Platform::new(Os::Darwin, Arch::X86_64);
        assert_eq!(
            RustupToolProvider::host_triple(&platform),
            "x86_64-apple-darwin"
        );

        // Linux + Arm64
        let platform = Platform::new(Os::Linux, Arch::Arm64);
        assert_eq!(
            RustupToolProvider::host_triple(&platform),
            "aarch64-unknown-linux-gnu"
        );

        // Linux + X86_64
        let platform = Platform::new(Os::Linux, Arch::X86_64);
        assert_eq!(
            RustupToolProvider::host_triple(&platform),
            "x86_64-unknown-linux-gnu"
        );
    }

    #[test]
    fn test_can_handle() {
        let provider = RustupToolProvider::new();

        let rustup_source = ToolSource::Rustup {
            toolchain: "1.83.0".into(),
            profile: Some("default".into()),
            components: vec![],
            targets: vec![],
        };
        assert!(provider.can_handle(&rustup_source));

        let github_source = ToolSource::GitHub {
            repo: "org/repo".into(),
            tag: "v1".into(),
            asset: "file.zip".into(),
            path: None,
        };
        assert!(!provider.can_handle(&github_source));
    }

    #[test]
    fn test_can_handle_nix_source() {
        let provider = RustupToolProvider::new();

        let nix_source = ToolSource::Nix {
            flake: "nixpkgs".into(),
            package: "cargo".into(),
            output: None,
        };
        assert!(!provider.can_handle(&nix_source));
    }

    #[test]
    fn test_can_handle_oci_source() {
        let provider = RustupToolProvider::new();

        let oci_source = ToolSource::Oci {
            image: "rust:latest".into(),
            path: "rust".into(),
        };
        assert!(!provider.can_handle(&oci_source));
    }

    #[test]
    fn test_compute_digest() {
        let digest1 = RustupToolProvider::compute_digest(
            "1.83.0",
            Some("default"),
            &["clippy".into(), "rustfmt".into()],
            &["x86_64-unknown-linux-gnu".into()],
        );
        assert!(digest1.starts_with("sha256:"));

        // Different config should produce different digest
        let digest2 = RustupToolProvider::compute_digest("1.83.0", Some("minimal"), &[], &[]);
        assert_ne!(digest1, digest2);

        // Same config should produce same digest
        let digest3 = RustupToolProvider::compute_digest(
            "1.83.0",
            Some("default"),
            &["clippy".into(), "rustfmt".into()],
            &["x86_64-unknown-linux-gnu".into()],
        );
        assert_eq!(digest1, digest3);
    }

    #[test]
    fn test_compute_digest_no_profile() {
        let digest = RustupToolProvider::compute_digest("stable", None, &[], &[]);
        assert!(digest.starts_with("sha256:"));
        // Default profile is used when None
        assert!(digest.len() > 10);
    }

    #[test]
    fn test_compute_digest_multiple_components() {
        let digest = RustupToolProvider::compute_digest(
            "nightly",
            Some("complete"),
            &[
                "clippy".into(),
                "rustfmt".into(),
                "rust-src".into(),
                "rust-analyzer".into(),
            ],
            &[],
        );
        assert!(digest.starts_with("sha256:"));
    }

    #[test]
    fn test_compute_digest_multiple_targets() {
        let digest = RustupToolProvider::compute_digest(
            "1.80.0",
            None,
            &[],
            &[
                "x86_64-unknown-linux-gnu".into(),
                "aarch64-unknown-linux-gnu".into(),
                "wasm32-unknown-unknown".into(),
            ],
        );
        assert!(digest.starts_with("sha256:"));
    }

    #[test]
    fn test_compute_digest_deterministic() {
        let digest1 = RustupToolProvider::compute_digest(
            "1.75.0",
            Some("default"),
            &["clippy".into()],
            &["x86_64-pc-windows-msvc".into()],
        );
        let digest2 = RustupToolProvider::compute_digest(
            "1.75.0",
            Some("default"),
            &["clippy".into()],
            &["x86_64-pc-windows-msvc".into()],
        );
        assert_eq!(digest1, digest2);
    }

    #[test]
    fn test_compute_digest_order_matters() {
        // Different component order should produce different digest
        let digest1 = RustupToolProvider::compute_digest(
            "stable",
            None,
            &["clippy".into(), "rustfmt".into()],
            &[],
        );
        let digest2 = RustupToolProvider::compute_digest(
            "stable",
            None,
            &["rustfmt".into(), "clippy".into()],
            &[],
        );
        assert_ne!(digest1, digest2);
    }

    #[test]
    fn test_toolchain_path() {
        let platform = Platform::new(Os::Darwin, Arch::Arm64);
        let path = RustupToolProvider::toolchain_path("1.83.0", &platform);

        // Should contain the toolchain and host triple
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("toolchains"));
        assert!(path_str.contains("1.83.0-aarch64-apple-darwin"));
    }

    #[test]
    fn test_toolchain_path_stable() {
        let platform = Platform::new(Os::Linux, Arch::X86_64);
        let path = RustupToolProvider::toolchain_path("stable", &platform);

        let path_str = path.to_string_lossy();
        assert!(path_str.contains("stable-x86_64-unknown-linux-gnu"));
    }

    #[test]
    fn test_toolchain_path_nightly() {
        let platform = Platform::new(Os::Darwin, Arch::X86_64);
        let path = RustupToolProvider::toolchain_path("nightly", &platform);

        let path_str = path.to_string_lossy();
        assert!(path_str.contains("nightly-x86_64-apple-darwin"));
    }

    #[test]
    fn test_is_toolchain_installed_nonexistent() {
        // A fake toolchain that definitely doesn't exist
        let platform = Platform::new(Os::Darwin, Arch::Arm64);
        let installed = RustupToolProvider::is_toolchain_installed(
            "nonexistent-fake-toolchain-12345",
            &platform,
        );
        assert!(!installed);
    }

    #[test]
    fn test_rustup_home_default() {
        // Test that rustup_home returns a path
        let home = RustupToolProvider::rustup_home();
        // Should end with .rustup when RUSTUP_HOME is not set
        // or be the RUSTUP_HOME value if set
        let path_str = home.to_string_lossy();
        assert!(path_str.contains("rustup") || path_str.contains(".rustup"));
    }

    #[tokio::test]
    async fn test_resolve_minimal_config() {
        let provider = RustupToolProvider::new();
        let platform = Platform::new(Os::Darwin, Arch::Arm64);
        let config = serde_json::json!({});

        let resolved = provider
            .resolve(&ToolResolveRequest {
                tool_name: "rust",
                version: "1.83.0",
                platform: &platform,
                config: &config,
                token: None,
            })
            .await;
        assert!(resolved.is_ok());

        let resolved = resolved.unwrap();
        assert_eq!(resolved.name, "rust");
        assert_eq!(resolved.version, "1.83.0");

        match &resolved.source {
            ToolSource::Rustup {
                toolchain,
                profile,
                components,
                targets,
            } => {
                assert_eq!(toolchain, "1.83.0");
                assert!(profile.is_none());
                assert!(components.is_empty());
                assert!(targets.is_empty());
            }
            _ => panic!("Expected Rustup source"),
        }
    }

    #[tokio::test]
    async fn test_resolve_with_toolchain() {
        let provider = RustupToolProvider::new();
        let platform = Platform::new(Os::Linux, Arch::X86_64);
        let config = serde_json::json!({
            "toolchain": "nightly"
        });

        let resolved = provider
            .resolve(&ToolResolveRequest {
                tool_name: "rust",
                version: "latest",
                platform: &platform,
                config: &config,
                token: None,
            })
            .await
            .unwrap();

        match &resolved.source {
            ToolSource::Rustup { toolchain, .. } => {
                assert_eq!(toolchain, "nightly");
            }
            _ => panic!("Expected Rustup source"),
        }
    }

    #[tokio::test]
    async fn test_resolve_with_profile() {
        let provider = RustupToolProvider::new();
        let platform = Platform::new(Os::Darwin, Arch::Arm64);
        let config = serde_json::json!({
            "profile": "minimal"
        });

        let resolved = provider
            .resolve(&ToolResolveRequest {
                tool_name: "rust",
                version: "1.80.0",
                platform: &platform,
                config: &config,
                token: None,
            })
            .await
            .unwrap();

        match &resolved.source {
            ToolSource::Rustup { profile, .. } => {
                assert_eq!(profile.as_deref(), Some("minimal"));
            }
            _ => panic!("Expected Rustup source"),
        }
    }

    #[tokio::test]
    async fn test_resolve_with_components() {
        let provider = RustupToolProvider::new();
        let platform = Platform::new(Os::Linux, Arch::Arm64);
        let config = serde_json::json!({
            "components": ["clippy", "rustfmt", "rust-src"]
        });

        let resolved = provider
            .resolve(&ToolResolveRequest {
                tool_name: "rust",
                version: "stable",
                platform: &platform,
                config: &config,
                token: None,
            })
            .await
            .unwrap();

        match &resolved.source {
            ToolSource::Rustup { components, .. } => {
                assert_eq!(components.len(), 3);
                assert!(components.contains(&"clippy".to_string()));
                assert!(components.contains(&"rustfmt".to_string()));
                assert!(components.contains(&"rust-src".to_string()));
            }
            _ => panic!("Expected Rustup source"),
        }
    }

    #[tokio::test]
    async fn test_resolve_with_targets() {
        let provider = RustupToolProvider::new();
        let platform = Platform::new(Os::Darwin, Arch::X86_64);
        let config = serde_json::json!({
            "targets": ["wasm32-unknown-unknown", "aarch64-apple-darwin"]
        });

        let resolved = provider
            .resolve(&ToolResolveRequest {
                tool_name: "rust",
                version: "1.82.0",
                platform: &platform,
                config: &config,
                token: None,
            })
            .await
            .unwrap();

        match &resolved.source {
            ToolSource::Rustup { targets, .. } => {
                assert_eq!(targets.len(), 2);
                assert!(targets.contains(&"wasm32-unknown-unknown".to_string()));
                assert!(targets.contains(&"aarch64-apple-darwin".to_string()));
            }
            _ => panic!("Expected Rustup source"),
        }
    }

    #[tokio::test]
    async fn test_resolve_full_config() {
        let provider = RustupToolProvider::new();
        let platform = Platform::new(Os::Linux, Arch::X86_64);
        let config = serde_json::json!({
            "toolchain": "nightly-2024-01-15",
            "profile": "complete",
            "components": ["clippy", "rustfmt", "rust-analyzer"],
            "targets": ["x86_64-unknown-linux-musl", "wasm32-wasi"]
        });

        let resolved = provider
            .resolve(&ToolResolveRequest {
                tool_name: "rust",
                version: "nightly",
                platform: &platform,
                config: &config,
                token: None,
            })
            .await
            .unwrap();

        match &resolved.source {
            ToolSource::Rustup {
                toolchain,
                profile,
                components,
                targets,
            } => {
                assert_eq!(toolchain, "nightly-2024-01-15");
                assert_eq!(profile.as_deref(), Some("complete"));
                assert_eq!(components.len(), 3);
                assert_eq!(targets.len(), 2);
            }
            _ => panic!("Expected Rustup source"),
        }
    }

    #[test]
    fn test_is_cached_wrong_source_type() {
        let provider = RustupToolProvider::new();
        let options = ToolOptions::new();

        let resolved = ResolvedTool {
            name: "sometool".to_string(),
            version: "1.0.0".to_string(),
            platform: Platform::new(Os::Darwin, Arch::Arm64),
            source: ToolSource::GitHub {
                repo: "owner/repo".to_string(),
                tag: "v1.0.0".to_string(),
                asset: "file.zip".to_string(),
                path: None,
            },
        };

        // Should return false for non-Rustup source
        assert!(!provider.is_cached(&resolved, &options));
    }
}
