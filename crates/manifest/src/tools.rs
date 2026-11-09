//! Tool provisioning DTOs.
//!
//! Platform triples, tool source definitions, extraction rules, and
//! activation steps shared between the lockfile schema and the tool
//! runtime in `cuenv-core`.

use serde::{Deserialize, Serialize};

/// Platform identifier combining OS and architecture.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Platform {
    /// Operating system component
    pub os: Os,
    /// CPU architecture component
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
    #[must_use]
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

    /// Expand `{version}`, `{os}`, and `{arch}` placeholders in a tool
    /// source template (GitHub tags/assets/paths, URLs, OCI image refs).
    ///
    /// `{os}` expands to `darwin`/`linux` and `{arch}` to `aarch64`/`x86_64`,
    /// so templates are portable across every tool provider.
    #[must_use]
    pub fn expand_template(&self, template: &str, version: &str) -> String {
        const VERSION_PLACEHOLDER: &str = "{version}";
        const OS_PLACEHOLDER: &str = "{os}";
        const ARCH_PLACEHOLDER: &str = "{arch}";

        let os = match self.os {
            Os::Darwin => "darwin",
            Os::Linux => "linux",
        };
        let arch = match self.arch {
            Arch::Arm64 => "aarch64",
            Arch::X86_64 => "x86_64",
        };
        template
            .replace(VERSION_PLACEHOLDER, version)
            .replace(OS_PLACEHOLDER, os)
            .replace(ARCH_PLACEHOLDER, arch)
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
    /// macOS
    Darwin,
    /// Linux
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
    /// 64-bit ARM (aarch64)
    Arm64,
    /// 64-bit x86 (amd64)
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
    Oci {
        /// Image reference (e.g., `ghcr.io/org/tool:1.2.3`)
        image: String,
        /// Path to the binary inside the image filesystem
        path: String,
    },
    /// Asset from a GitHub release.
    GitHub {
        /// Repository in `owner/name` form
        repo: String,
        /// Release tag
        tag: String,
        /// Asset filename within the release
        asset: String,
        /// Typed extraction rules for archive/binary assets.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        extract: Vec<ToolExtract>,
    },
    /// Package from a Nix flake.
    Nix {
        /// Flake reference (e.g., `nixpkgs`)
        flake: String,
        /// Package attribute within the flake
        package: String,
        /// Specific flake output to install
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
    /// Asset from an arbitrary HTTP URL.
    #[serde(rename = "url")]
    Url {
        /// Fully-resolved download URL.
        url: String,
        /// Typed extraction rules for archive/binary assets.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        extract: Vec<ToolExtract>,
    },
}

/// Typed extract rule for GitHub release assets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ToolExtract {
    /// Extract to `bin/`.
    Bin {
        /// Path within archive/pkg payload.
        path: String,
        /// Optional binary rename.
        #[serde(rename = "as", skip_serializing_if = "Option::is_none")]
        as_name: Option<String>,
    },
    /// Extract to `lib/`.
    Lib {
        /// Path within archive/pkg payload.
        path: String,
        /// Optional env var for exact path export.
        #[serde(skip_serializing_if = "Option::is_none")]
        env: Option<String>,
    },
    /// Extract to `include/`.
    Include {
        /// Path within archive/pkg payload.
        path: String,
    },
    /// Extract to `lib/pkgconfig/`.
    PkgConfig {
        /// Path within archive/pkg payload.
        path: String,
    },
    /// Extract to `files/`.
    File {
        /// Path within archive/pkg payload.
        path: String,
        /// Optional env var for exact path export.
        #[serde(skip_serializing_if = "Option::is_none")]
        env: Option<String>,
    },
}

impl ToolExtract {
    /// Return a copy with `{version}`, `{os}`, and `{arch}` placeholders in
    /// the extraction path expanded for `platform`.
    #[must_use]
    pub fn expand_template(&self, platform: &Platform, version: &str) -> Self {
        match self {
            Self::Bin { path, as_name } => Self::Bin {
                path: platform.expand_template(path, version),
                as_name: as_name.clone(),
            },
            Self::Lib { path, env } => Self::Lib {
                path: platform.expand_template(path, version),
                env: env.clone(),
            },
            Self::Include { path } => Self::Include {
                path: platform.expand_template(path, version),
            },
            Self::PkgConfig { path } => Self::PkgConfig {
                path: platform.expand_template(path, version),
            },
            Self::File { path, env } => Self::File {
                path: platform.expand_template(path, version),
                env: env.clone(),
            },
        }
    }

    /// Where this extracted artifact lives inside a tool's cache directory.
    ///
    /// Binaries land in `bin/` (renamed by `as` when set), libraries in
    /// `lib/`, headers in `include/`, pkg-config files in `lib/pkgconfig/`,
    /// and other files in `files/`. `tool_name` is the fallback file name
    /// when the extraction path has none.
    #[must_use]
    pub fn cache_target(&self, cache_dir: &std::path::Path, tool_name: &str) -> std::path::PathBuf {
        let file_name = |path: &str| -> String {
            std::path::Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(tool_name)
                .to_string()
        };
        match self {
            Self::Bin { path, as_name } => cache_dir
                .join("bin")
                .join(as_name.clone().unwrap_or_else(|| file_name(path))),
            Self::Lib { path, .. } => cache_dir.join("lib").join(file_name(path)),
            Self::Include { path } => cache_dir.join("include").join(file_name(path)),
            Self::PkgConfig { path } => cache_dir
                .join("lib")
                .join("pkgconfig")
                .join(file_name(path)),
            Self::File { path, .. } => cache_dir.join("files").join(file_name(path)),
        }
    }
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
            Self::Url { .. } => "url",
        }
    }
}

/// A configured activation step from runtime/lockfile configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolActivationStep {
    /// Environment variable to mutate (for example `PATH`).
    pub var: String,
    /// Mutation operation.
    pub op: ToolActivationOperation,
    /// Separator for joining values (defaults to `:`).
    #[serde(default = "default_separator")]
    pub separator: String,
    /// Source reference that resolves to one or more paths.
    pub from: ToolActivationSource,
}

fn default_separator() -> String {
    ":".to_string()
}

/// Mutation operation for tool activation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ToolActivationOperation {
    /// Replace the variable with the resolved value.
    Set,
    /// Prepend the resolved value before the current value.
    Prepend,
    /// Append the resolved value after the current value.
    Append,
}

/// Activation source selector.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolActivationSource {
    /// All bin directories for tools available on the current platform.
    AllBinDirs,
    /// All lib directories for tools available on the current platform.
    AllLibDirs,
    /// Bin directory for a specific tool.
    ToolBinDir {
        /// Tool name whose bin directory is referenced
        tool: String,
    },
    /// Lib directory for a specific tool.
    ToolLibDir {
        /// Tool name whose lib directory is referenced
        tool: String,
    },
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
