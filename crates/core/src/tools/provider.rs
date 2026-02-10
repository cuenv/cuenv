//! Tool provider trait for extensible tool fetching.
//!
//! This module defines the `ToolProvider` trait that allows different sources
//! (GitHub releases, Nix packages, OCI images) to be registered and used
//! uniformly for fetching development tools.

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
    /// Binary extracted from an OCI container image.
    Oci { image: String, path: String },
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
    /// Rust toolchain managed by rustup.
    Rustup {
        /// Toolchain identifier (e.g., "stable", "1.83.0", "nightly-2024-01-01").
        toolchain: String,
        /// Installation profile: minimal, default, complete.
        #[serde(skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
        /// Additional components to install (e.g., "clippy", "rustfmt", "rust-src").
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        components: Vec<String>,
        /// Additional targets to install (e.g., "x86_64-unknown-linux-gnu").
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        targets: Vec<String>,
    },
}

impl ToolSource {
    /// Get the provider type name.
    #[must_use]
    pub fn provider_type(&self) -> &'static str {
        match self {
            Self::Oci { .. } => "oci",
            Self::GitHub { .. } => "github",
            Self::Nix { .. } => "nix",
            Self::Rustup { .. } => "rustup",
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

/// Request parameters for tool resolution.
pub struct ToolResolveRequest<'a> {
    /// Name of the tool (e.g., "jq").
    pub tool_name: &'a str,
    /// Version string from the manifest.
    pub version: &'a str,
    /// Target platform.
    pub platform: &'a Platform,
    /// Provider-specific configuration from CUE.
    pub config: &'a serde_json::Value,
    /// Optional authentication token (e.g., GitHub token for rate limiting).
    pub token: Option<&'a str>,
}

/// Trait for tool providers (GitHub, OCI, Nix).
///
/// Each provider implements this trait to handle resolution and fetching
/// of tools from a specific source type. Providers are registered with
/// the `ToolRegistry` and selected based on the source configuration.
///
/// # Example
///
/// ```ignore
/// pub struct GitHubToolProvider { /* ... */ }
///
/// #[async_trait]
/// impl ToolProvider for GitHubToolProvider {
///     fn name(&self) -> &'static str { "github" }
///     fn description(&self) -> &'static str { "Fetch tools from GitHub releases" }
///     // ...
/// }
/// ```
#[async_trait]
pub trait ToolProvider: Send + Sync {
    /// Provider name (e.g., "github", "nix", "oci").
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
    /// * `request` - Resolution parameters including tool name, version, platform,
    ///   provider-specific config, and optional authentication token
    ///
    /// # Errors
    ///
    /// Returns an error if resolution fails (version not found, etc.)
    async fn resolve(&self, request: &ToolResolveRequest<'_>) -> Result<ResolvedTool>;

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
    fn test_platform_parse_edge_cases() {
        // Too few parts
        assert!(Platform::parse("darwin").is_none());
        // Too many parts
        assert!(Platform::parse("darwin-arm64-extra").is_none());
        // Empty string
        assert!(Platform::parse("").is_none());
        // Invalid OS
        assert!(Platform::parse("windows-arm64").is_none());
        // Invalid arch
        assert!(Platform::parse("darwin-mips").is_none());
    }

    #[test]
    fn test_platform_display() {
        let p = Platform::new(Os::Darwin, Arch::Arm64);
        assert_eq!(p.to_string(), "darwin-arm64");
    }

    #[test]
    fn test_platform_display_all_combinations() {
        assert_eq!(
            Platform::new(Os::Darwin, Arch::Arm64).to_string(),
            "darwin-arm64"
        );
        assert_eq!(
            Platform::new(Os::Darwin, Arch::X86_64).to_string(),
            "darwin-x86_64"
        );
        assert_eq!(
            Platform::new(Os::Linux, Arch::Arm64).to_string(),
            "linux-arm64"
        );
        assert_eq!(
            Platform::new(Os::Linux, Arch::X86_64).to_string(),
            "linux-x86_64"
        );
    }

    #[test]
    fn test_platform_current() {
        let p = Platform::current();
        // Should return a valid platform for the current system
        assert!(matches!(p.os, Os::Darwin | Os::Linux));
        assert!(matches!(p.arch, Arch::Arm64 | Arch::X86_64));
    }

    #[test]
    fn test_os_parse() {
        assert_eq!(Os::parse("darwin"), Some(Os::Darwin));
        assert_eq!(Os::parse("macos"), Some(Os::Darwin));
        assert_eq!(Os::parse("linux"), Some(Os::Linux));
        assert_eq!(Os::parse("windows"), None);
    }

    #[test]
    fn test_os_parse_case_insensitive() {
        assert_eq!(Os::parse("DARWIN"), Some(Os::Darwin));
        assert_eq!(Os::parse("Darwin"), Some(Os::Darwin));
        assert_eq!(Os::parse("LINUX"), Some(Os::Linux));
        assert_eq!(Os::parse("Linux"), Some(Os::Linux));
        assert_eq!(Os::parse("MACOS"), Some(Os::Darwin));
        assert_eq!(Os::parse("MacOS"), Some(Os::Darwin));
    }

    #[test]
    fn test_os_display() {
        assert_eq!(Os::Darwin.to_string(), "darwin");
        assert_eq!(Os::Linux.to_string(), "linux");
    }

    #[test]
    fn test_os_current() {
        let os = Os::current();
        // Should return a valid OS for the current system
        assert!(matches!(os, Os::Darwin | Os::Linux));
    }

    #[test]
    fn test_arch_parse() {
        assert_eq!(Arch::parse("arm64"), Some(Arch::Arm64));
        assert_eq!(Arch::parse("aarch64"), Some(Arch::Arm64));
        assert_eq!(Arch::parse("x86_64"), Some(Arch::X86_64));
        assert_eq!(Arch::parse("amd64"), Some(Arch::X86_64));
    }

    #[test]
    fn test_arch_parse_case_insensitive() {
        assert_eq!(Arch::parse("ARM64"), Some(Arch::Arm64));
        assert_eq!(Arch::parse("Arm64"), Some(Arch::Arm64));
        assert_eq!(Arch::parse("AARCH64"), Some(Arch::Arm64));
        assert_eq!(Arch::parse("X86_64"), Some(Arch::X86_64));
        assert_eq!(Arch::parse("AMD64"), Some(Arch::X86_64));
    }

    #[test]
    fn test_arch_parse_x64_alias() {
        assert_eq!(Arch::parse("x64"), Some(Arch::X86_64));
        assert_eq!(Arch::parse("X64"), Some(Arch::X86_64));
    }

    #[test]
    fn test_arch_parse_invalid() {
        assert!(Arch::parse("mips").is_none());
        assert!(Arch::parse("riscv").is_none());
        assert!(Arch::parse("").is_none());
    }

    #[test]
    fn test_arch_display() {
        assert_eq!(Arch::Arm64.to_string(), "arm64");
        assert_eq!(Arch::X86_64.to_string(), "x86_64");
    }

    #[test]
    fn test_arch_current() {
        let arch = Arch::current();
        // Should return a valid arch for the current system
        assert!(matches!(arch, Arch::Arm64 | Arch::X86_64));
    }

    #[test]
    fn test_tool_source_provider_type() {
        let s = ToolSource::GitHub {
            repo: "jqlang/jq".into(),
            tag: "jq-1.7.1".into(),
            asset: "jq-macos-arm64".into(),
            path: None,
        };
        assert_eq!(s.provider_type(), "github");

        let s = ToolSource::Nix {
            flake: "nixpkgs".into(),
            package: "jq".into(),
            output: None,
        };
        assert_eq!(s.provider_type(), "nix");

        let s = ToolSource::Rustup {
            toolchain: "1.83.0".into(),
            profile: Some("default".into()),
            components: vec!["clippy".into(), "rustfmt".into()],
            targets: vec!["x86_64-unknown-linux-gnu".into()],
        };
        assert_eq!(s.provider_type(), "rustup");
    }

    #[test]
    fn test_tool_source_oci_provider_type() {
        let s = ToolSource::Oci {
            image: "docker.io/library/alpine:latest".into(),
            path: "/usr/bin/jq".into(),
        };
        assert_eq!(s.provider_type(), "oci");
    }

    #[test]
    fn test_tool_source_serialization() {
        let source = ToolSource::GitHub {
            repo: "jqlang/jq".into(),
            tag: "jq-1.7.1".into(),
            asset: "jq-macos-arm64".into(),
            path: Some("jq-macos-arm64/jq".into()),
        };
        let json = serde_json::to_string(&source).unwrap();
        assert!(json.contains("\"type\":\"github\""));
        assert!(json.contains("\"repo\":\"jqlang/jq\""));
        assert!(json.contains("\"path\":\"jq-macos-arm64/jq\""));
    }

    #[test]
    fn test_tool_source_deserialization() {
        let json =
            r#"{"type":"github","repo":"jqlang/jq","tag":"jq-1.7.1","asset":"jq-macos-arm64"}"#;
        let source: ToolSource = serde_json::from_str(json).unwrap();
        match source {
            ToolSource::GitHub {
                repo, tag, asset, ..
            } => {
                assert_eq!(repo, "jqlang/jq");
                assert_eq!(tag, "jq-1.7.1");
                assert_eq!(asset, "jq-macos-arm64");
            }
            _ => panic!("Expected GitHub source"),
        }
    }

    #[test]
    fn test_tool_source_nix_serialization() {
        let source = ToolSource::Nix {
            flake: "nixpkgs".into(),
            package: "jq".into(),
            output: Some("bin".into()),
        };
        let json = serde_json::to_string(&source).unwrap();
        assert!(json.contains("\"type\":\"nix\""));
        assert!(json.contains("\"output\":\"bin\""));
    }

    #[test]
    fn test_tool_source_rustup_serialization() {
        let source = ToolSource::Rustup {
            toolchain: "stable".into(),
            profile: None,
            components: vec![],
            targets: vec![],
        };
        let json = serde_json::to_string(&source).unwrap();
        assert!(json.contains("\"type\":\"rustup\""));
        // Empty vecs should not be serialized
        assert!(!json.contains("components"));
        assert!(!json.contains("targets"));
    }

    #[test]
    fn test_resolved_tool_serialization() {
        let tool = ResolvedTool {
            name: "jq".into(),
            version: "1.7.1".into(),
            platform: Platform::new(Os::Darwin, Arch::Arm64),
            source: ToolSource::GitHub {
                repo: "jqlang/jq".into(),
                tag: "jq-1.7.1".into(),
                asset: "jq-macos-arm64".into(),
                path: None,
            },
        };
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains("\"name\":\"jq\""));
        assert!(json.contains("\"version\":\"1.7.1\""));
    }

    #[test]
    fn test_tool_options_default() {
        let opts = ToolOptions::default();
        assert!(opts.cache_dir.is_none());
        assert!(!opts.force_refetch);
    }

    #[test]
    fn test_tool_options_new() {
        let opts = ToolOptions::new();
        assert!(opts.cache_dir.is_none());
        assert!(!opts.force_refetch);
    }

    #[test]
    fn test_tool_options_builder() {
        let opts = ToolOptions::new()
            .with_cache_dir(PathBuf::from("/custom/cache"))
            .with_force_refetch(true);

        assert_eq!(opts.cache_dir, Some(PathBuf::from("/custom/cache")));
        assert!(opts.force_refetch);
    }

    #[test]
    fn test_tool_options_cache_dir_default() {
        let opts = ToolOptions::new();
        let cache_dir = opts.cache_dir();
        // Should end with cuenv/tools
        assert!(cache_dir.ends_with("cuenv/tools"));
    }

    #[test]
    fn test_tool_options_cache_dir_custom() {
        let opts = ToolOptions::new().with_cache_dir(PathBuf::from("/my/cache"));
        assert_eq!(opts.cache_dir(), PathBuf::from("/my/cache"));
    }

    #[test]
    fn test_default_cache_dir() {
        let cache_dir = default_cache_dir();
        // Should end with cuenv/tools
        assert!(cache_dir.ends_with("cuenv/tools"));
    }

    #[test]
    fn test_platform_equality() {
        let p1 = Platform::new(Os::Darwin, Arch::Arm64);
        let p2 = Platform::new(Os::Darwin, Arch::Arm64);
        let p3 = Platform::new(Os::Linux, Arch::Arm64);

        assert_eq!(p1, p2);
        assert_ne!(p1, p3);
    }

    #[test]
    fn test_platform_hash() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(Platform::new(Os::Darwin, Arch::Arm64));
        set.insert(Platform::new(Os::Darwin, Arch::Arm64)); // Duplicate

        assert_eq!(set.len(), 1);

        set.insert(Platform::new(Os::Linux, Arch::Arm64));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn test_os_equality() {
        assert_eq!(Os::Darwin, Os::Darwin);
        assert_eq!(Os::Linux, Os::Linux);
        assert_ne!(Os::Darwin, Os::Linux);
    }

    #[test]
    fn test_arch_equality() {
        assert_eq!(Arch::Arm64, Arch::Arm64);
        assert_eq!(Arch::X86_64, Arch::X86_64);
        assert_ne!(Arch::Arm64, Arch::X86_64);
    }
}
