//! Tool provider trait for extensible tool fetching.
//!
//! This module defines the `ToolProvider` trait that allows different sources
//! (Homebrew, Docker, GitHub, Nix) to be registered and used uniformly for
//! fetching development tools.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::Result;

/// Platform identifier combining OS and architecture.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Platform {
    pub os: Os,
    pub arch: Arch,
}

impl Platform {
    /// Create a new platform.
    #[must_use]
    pub fn new(os: Os, arch: Arch) -> Self {
        Self { os, arch }
    }

    /// Get the current platform.
    #[must_use]
    pub fn current() -> Self {
        Self {
            os: Os::current(),
            arch: Arch::current(),
        }
    }

    /// Parse from string like "darwin-arm64".
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() != 2 {
            return None;
        }
        Some(Self {
            os: Os::parse(parts[0])?,
            arch: Arch::parse(parts[1])?,
        })
    }
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}", self.os, self.arch)
    }
}

/// Operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Darwin,
    Linux,
}

impl Os {
    /// Get the current OS.
    #[must_use]
    pub fn current() -> Self {
        #[cfg(target_os = "macos")]
        return Self::Darwin;
        #[cfg(target_os = "linux")]
        return Self::Linux;
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        compile_error!("Unsupported OS");
    }

    /// Parse from string.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "darwin" | "macos" => Some(Self::Darwin),
            "linux" => Some(Self::Linux),
            _ => None,
        }
    }
}

impl std::fmt::Display for Os {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Darwin => write!(f, "darwin"),
            Self::Linux => write!(f, "linux"),
        }
    }
}

/// CPU architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    Arm64,
    X86_64,
}

impl Arch {
    /// Get the current architecture.
    #[must_use]
    pub fn current() -> Self {
        #[cfg(target_arch = "aarch64")]
        return Self::Arm64;
        #[cfg(target_arch = "x86_64")]
        return Self::X86_64;
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        compile_error!("Unsupported architecture");
    }

    /// Parse from string.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "arm64" | "aarch64" => Some(Self::Arm64),
            "x86_64" | "amd64" | "x64" => Some(Self::X86_64),
            _ => None,
        }
    }
}

impl std::fmt::Display for Arch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Arm64 => write!(f, "arm64"),
            Self::X86_64 => write!(f, "x86_64"),
        }
    }
}

/// Source-specific resolution data.
///
/// This enum contains the provider-specific information needed to fetch a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolSource {
    /// Homebrew bottle from ghcr.io/homebrew.
    Homebrew {
        formula: String,
        image_ref: String,
    },
    /// Binary extracted from an OCI container image.
    Oci {
        image: String,
        path: String,
    },
    /// Asset from a GitHub release.
    GitHub {
        repo: String,
        tag: String,
        asset: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// Package from a Nix flake.
    Nix {
        flake: String,
        package: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
}

impl ToolSource {
    /// Get the provider type name.
    #[must_use]
    pub fn provider_type(&self) -> &'static str {
        match self {
            Self::Homebrew { .. } => "homebrew",
            Self::Oci { .. } => "oci",
            Self::GitHub { .. } => "github",
            Self::Nix { .. } => "nix",
        }
    }
}

/// A resolved tool ready to be fetched.
///
/// This represents a fully resolved tool specification with all information
/// needed to download and cache the binary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedTool {
    /// Tool name (e.g., "jq", "bun").
    pub name: String,
    /// Version string.
    pub version: String,
    /// Target platform.
    pub platform: Platform,
    /// Source-specific data.
    pub source: ToolSource,
}

/// Result of fetching a tool.
#[derive(Debug)]
pub struct FetchedTool {
    /// Tool name.
    pub name: String,
    /// Path to the cached binary.
    pub binary_path: PathBuf,
    /// SHA256 hash of the binary.
    pub sha256: String,
}

/// Options for tool operations.
#[derive(Debug, Clone, Default)]
pub struct ToolOptions {
    /// Custom cache directory.
    pub cache_dir: Option<PathBuf>,
    /// Force re-fetch even if cached.
    pub force_refetch: bool,
}

impl ToolOptions {
    /// Create new options with default cache directory.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the cache directory.
    #[must_use]
    pub fn with_cache_dir(mut self, path: PathBuf) -> Self {
        self.cache_dir = Some(path);
        self
    }

    /// Set force refetch.
    #[must_use]
    pub fn with_force_refetch(mut self, force: bool) -> Self {
        self.force_refetch = force;
        self
    }

    /// Get the cache directory, defaulting to ~/.cache/cuenv/tools.
    #[must_use]
    pub fn cache_dir(&self) -> PathBuf {
        self.cache_dir.clone().unwrap_or_else(default_cache_dir)
    }
}

/// Get the default cache directory for tools.
#[must_use]
pub fn default_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("cuenv")
        .join("tools")
}

/// Trait for tool providers (Homebrew, Docker, GitHub, Nix).
///
/// Each provider implements this trait to handle resolution and fetching
/// of tools from a specific source type. Providers are registered with
/// the `ToolRegistry` and selected based on the source configuration.
///
/// # Example
///
/// ```ignore
/// pub struct HomebrewToolProvider { /* ... */ }
///
/// #[async_trait]
/// impl ToolProvider for HomebrewToolProvider {
///     fn name(&self) -> &'static str { "homebrew" }
///     fn description(&self) -> &'static str { "Fetch tools from Homebrew bottles" }
///     // ...
/// }
/// ```
#[async_trait]
pub trait ToolProvider: Send + Sync {
    /// Provider name (e.g., "homebrew", "github", "nix").
    ///
    /// This should match the `type` field in the CUE schema.
    fn name(&self) -> &'static str;

    /// Human-readable description for help text.
    fn description(&self) -> &'static str;

    /// Check if this provider can handle the given source type.
    fn can_handle(&self, source: &ToolSource) -> bool;

    /// Resolve a tool specification to a fetchable artifact.
    ///
    /// This performs version resolution, platform matching, and returns
    /// the concrete artifact reference (image digest, release URL, etc.)
    ///
    /// # Arguments
    ///
    /// * `tool_name` - Name of the tool (e.g., "jq")
    /// * `version` - Version string from the manifest
    /// * `platform` - Target platform
    /// * `config` - Provider-specific configuration from CUE
    ///
    /// # Errors
    ///
    /// Returns an error if resolution fails (version not found, etc.)
    async fn resolve(
        &self,
        tool_name: &str,
        version: &str,
        platform: &Platform,
        config: &serde_json::Value,
    ) -> Result<ResolvedTool>;

    /// Fetch and cache a resolved tool.
    ///
    /// Downloads the artifact, extracts binaries, and returns the local path.
    /// If the tool is already cached and `force_refetch` is false, returns
    /// the cached path without re-downloading.
    ///
    /// # Arguments
    ///
    /// * `resolved` - A previously resolved tool
    /// * `options` - Fetch options (cache dir, force refetch)
    ///
    /// # Errors
    ///
    /// Returns an error if fetching or extraction fails.
    async fn fetch(&self, resolved: &ResolvedTool, options: &ToolOptions) -> Result<FetchedTool>;

    /// Check if a tool is already cached.
    ///
    /// Returns true if the tool binary exists in the cache directory.
    fn is_cached(&self, resolved: &ResolvedTool, options: &ToolOptions) -> bool;

    /// Check if provider prerequisites are available.
    ///
    /// Called early during runtime activation to fail fast if required
    /// dependencies are missing (e.g., Nix CLI not installed).
    ///
    /// # Default Implementation
    ///
    /// Returns `Ok(())` - most providers only need HTTP access.
    ///
    /// # Errors
    ///
    /// Returns an error with a helpful message if prerequisites are not met.
    async fn check_prerequisites(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_parse() {
        let p = Platform::parse("darwin-arm64").unwrap();
        assert_eq!(p.os, Os::Darwin);
        assert_eq!(p.arch, Arch::Arm64);

        let p = Platform::parse("linux-x86_64").unwrap();
        assert_eq!(p.os, Os::Linux);
        assert_eq!(p.arch, Arch::X86_64);

        assert!(Platform::parse("invalid").is_none());
    }

    #[test]
    fn test_platform_display() {
        let p = Platform::new(Os::Darwin, Arch::Arm64);
        assert_eq!(p.to_string(), "darwin-arm64");
    }

    #[test]
    fn test_os_parse() {
        assert_eq!(Os::parse("darwin"), Some(Os::Darwin));
        assert_eq!(Os::parse("macos"), Some(Os::Darwin));
        assert_eq!(Os::parse("linux"), Some(Os::Linux));
        assert_eq!(Os::parse("windows"), None);
    }

    #[test]
    fn test_arch_parse() {
        assert_eq!(Arch::parse("arm64"), Some(Arch::Arm64));
        assert_eq!(Arch::parse("aarch64"), Some(Arch::Arm64));
        assert_eq!(Arch::parse("x86_64"), Some(Arch::X86_64));
        assert_eq!(Arch::parse("amd64"), Some(Arch::X86_64));
    }

    #[test]
    fn test_tool_source_provider_type() {
        let s = ToolSource::Homebrew {
            formula: "jq".into(),
            image_ref: "ghcr.io/homebrew/core/jq:1.7.1".into(),
        };
        assert_eq!(s.provider_type(), "homebrew");
    }
}
