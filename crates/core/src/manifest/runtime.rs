use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::secrets::Secret;

// ============================================================================
// Runtime Types
// ============================================================================

/// Runtime declares where/how a task executes.
/// Set at project level as the default, override per-task as needed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Runtime {
    /// Activate Nix devShell before execution
    Nix(NixRuntime),
    /// Activate devenv shell before execution
    Devenv(DevenvRuntime),
    /// Simple container execution
    Container(ContainerRuntime),
    /// Advanced container with caching, secrets, chaining
    Dagger(DaggerRuntime),
    /// OCI-based binary fetching from container images
    Oci(OciRuntime),
    /// Multi-source tool management (GitHub, OCI, Nix)
    Tools(Box<ToolsRuntime>),
}

/// Nix runtime configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NixRuntime {
    /// Flake reference (default: "." for local flake.nix)
    #[serde(default = "default_flake")]
    pub flake: String,
    /// Output attribute path (default: devShells.${system}.default)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

impl Default for NixRuntime {
    fn default() -> Self {
        Self {
            flake: default_flake(),
            output: None,
        }
    }
}

fn default_flake() -> String {
    ".".to_string()
}

/// Devenv runtime configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DevenvRuntime {
    /// Path to devenv config directory (default: ".")
    #[serde(default = "default_flake")]
    pub path: String,
}

/// Simple container runtime configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContainerRuntime {
    /// Container image (e.g., "node:20-alpine", "rust:1.75-slim")
    pub image: String,
}

/// Dagger runtime configuration (advanced container orchestration)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DaggerRuntime {
    /// Base container image (required unless 'from' is specified)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Use container from a previous task as base
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Secrets to mount or expose as environment variables
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<DaggerSecret>,
    /// Cache volumes for persistent build caching
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache: Vec<DaggerCacheMount>,
}

/// Secret configuration for Dagger containers
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaggerSecret {
    /// Name identifier for the secret
    pub name: String,
    /// Mount secret as a file at this path
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Expose secret as an environment variable with this name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
    /// Secret resolver configuration
    pub resolver: serde_json::Value,
}

/// Cache volume mount configuration for Dagger
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaggerCacheMount {
    /// Path inside the container to mount the cache
    pub path: String,
    /// Unique name for the cache volume
    pub name: String,
}

/// OCI-based binary runtime configuration.
///
/// Fetches binaries from OCI images for hermetic, content-addressed binary management.
/// Images require explicit `extract` paths to specify which binaries to extract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct OciRuntime {
    /// Platforms to resolve and lock (e.g., "darwin-arm64", "linux-x86_64")
    #[serde(default)]
    pub platforms: Vec<String>,
    /// OCI images to fetch binaries from
    #[serde(default)]
    pub images: Vec<OciImage>,
    /// Cache directory (defaults to ~/.cache/cuenv/oci)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_dir: Option<String>,
}

/// An OCI image to extract binaries from.
///
/// Images require explicit `extract` paths to specify which binaries to extract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OciImage {
    /// Full image reference (e.g., "nginx:1.25-alpine", "gcr.io/distroless/static:latest")
    pub image: String,
    /// Rename the extracted binary (when package name differs from binary name)
    #[serde(rename = "as", skip_serializing_if = "Option::is_none")]
    pub as_name: Option<String>,
    /// Extraction paths specifying which binaries to extract from the image
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extract: Vec<OciExtract>,
}

/// A binary to extract from a container image.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OciExtract {
    /// Path to the binary inside the container (e.g., "/usr/sbin/nginx")
    pub path: String,
    /// Name to expose the binary as in PATH (defaults to filename from path)
    #[serde(rename = "as", skip_serializing_if = "Option::is_none")]
    pub as_name: Option<String>,
}

/// GitHub provider configuration for runtime-level authentication.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GitHubProviderConfig {
    /// Authentication token (must use secret resolver like 1Password or exec)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<Secret>,
}

/// Multi-source tool runtime configuration.
///
/// Provides ergonomic tool management with platform-specific overrides.
/// Simple case: `jq: "1.7.1"` requires a source to be defined.
/// Complex case: Platform-specific sources with overrides.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolsRuntime {
    /// Platforms to resolve and lock (e.g., "darwin-arm64", "linux-x86_64")
    #[serde(default)]
    pub platforms: Vec<String>,
    /// Named Nix flake references for pinning
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub flakes: HashMap<String, String>,
    /// GitHub provider configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github: Option<GitHubProviderConfig>,
    /// Tool specifications (version string or full Tool config)
    #[serde(default)]
    pub tools: HashMap<String, ToolSpec>,
    /// Cache directory (defaults to ~/.cache/cuenv/tools)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_dir: Option<String>,
}

/// Tool specification - either a simple version or full config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ToolSpec {
    /// Simple version string (requires explicit source configuration)
    Version(String),
    /// Full tool configuration with source and overrides
    Full(ToolConfig),
}

impl ToolSpec {
    /// Get the version string.
    #[must_use]
    pub fn version(&self) -> &str {
        match self {
            Self::Version(v) => v,
            Self::Full(c) => &c.version,
        }
    }
}

/// Full tool configuration with source and platform overrides.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfig {
    /// Version string (e.g., "1.7.1", "latest")
    pub version: String,
    /// Rename the binary in PATH
    #[serde(rename = "as", skip_serializing_if = "Option::is_none")]
    pub as_name: Option<String>,
    /// Default source for all platforms
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceConfig>,
    /// Platform-specific source overrides
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<SourceOverride>,
}

/// Platform-specific source override.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceOverride {
    /// Match by OS (darwin, linux)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// Match by architecture (arm64, x86_64)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    /// Source for matching platforms
    pub source: SourceConfig,
}

/// Source configuration for fetching a tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SourceConfig {
    /// Extract from OCI container image
    Oci {
        /// Image reference with optional {version}, {os}, {arch} templates
        image: String,
        /// Path to binary inside the container
        path: String,
    },
    /// Download from GitHub Releases
    #[serde(rename = "github")]
    GitHub {
        /// Repository (owner/repo)
        repo: String,
        /// Tag prefix (prepended to version, defaults to "")
        #[serde(default, rename = "tagPrefix")]
        tag_prefix: String,
        /// Release tag override (if set, ignores tagPrefix)
        #[serde(skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        /// Asset name with optional {version}, {os}, {arch} templates
        asset: String,
        /// Legacy single-file selector inside archive/pkg payloads.
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// Optional typed extraction rules for archive/pkg assets.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        extract: Vec<GitHubExtract>,
    },
    /// Build from Nix flake
    Nix {
        /// Named flake reference (key in runtime.flakes)
        flake: String,
        /// Package attribute (e.g., "jq", "python3")
        package: String,
        /// Output path if binary can't be auto-detected
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    /// Install via rustup
    Rustup {
        /// Toolchain identifier (e.g., "stable", "1.83.0", "nightly-2024-01-01")
        toolchain: String,
        /// Installation profile: minimal, default, complete
        #[serde(default = "default_rustup_profile")]
        profile: String,
        /// Additional components to install (e.g., "clippy", "rustfmt", "rust-src")
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        components: Vec<String>,
        /// Additional targets to install (e.g., "x86_64-unknown-linux-gnu")
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        targets: Vec<String>,
    },
    /// Download from an arbitrary HTTP URL
    #[serde(rename = "url")]
    Url {
        /// URL with optional {version}, {os}, {arch} templates
        url: String,
        /// Legacy single-file selector inside archive payloads.
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// Optional typed extraction rules for archive assets.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        extract: Vec<GitHubExtract>,
    },
}

fn default_rustup_profile() -> String {
    "default".to_string()
}

/// Typed extraction rule for GitHub release assets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum GitHubExtract {
    /// Extract a binary and place it in `bin/`.
    Bin {
        /// Path to file in the archive/pkg payload.
        path: String,
        /// Optional binary rename in cache/bin.
        #[serde(rename = "as", skip_serializing_if = "Option::is_none")]
        as_name: Option<String>,
    },
    /// Extract a dynamic library and place it in `lib/`.
    Lib {
        /// Path to file in the archive/pkg payload.
        path: String,
        /// Optional env var to export the absolute file path.
        #[serde(skip_serializing_if = "Option::is_none")]
        env: Option<String>,
    },
    /// Extract include/header material and place it in `include/`.
    Include {
        /// Path to file in the archive/pkg payload.
        path: String,
    },
    /// Extract pkg-config metadata and place it in `lib/pkgconfig/`.
    PkgConfig {
        /// Path to file in the archive/pkg payload.
        path: String,
    },
    /// Extract a generic file and place it in `files/`.
    File {
        /// Path to file in the archive/pkg payload.
        path: String,
        /// Optional env var to export the absolute file path.
        #[serde(skip_serializing_if = "Option::is_none")]
        env: Option<String>,
    },
}
