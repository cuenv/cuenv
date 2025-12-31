//! Platform detection and normalization.
//!
//! Handles mapping between:
//! - cuenv platform strings (e.g., "darwin-arm64", "linux-x86_64")
//! - OCI platform specs (os/arch, e.g., "darwin/arm64", "linux/amd64")

use std::fmt;

/// A normalized platform specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Platform {
    /// Operating system (darwin, linux, windows).
    pub os: String,
    /// Architecture (arm64, x86_64).
    pub arch: String,
}

impl Platform {
    /// Create a new platform.
    #[must_use]
    pub fn new(os: impl Into<String>, arch: impl Into<String>) -> Self {
        Self {
            os: os.into(),
            arch: arch.into(),
        }
    }

    /// Parse a cuenv platform string (e.g., "darwin-arm64").
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let normalized = normalize_platform(s);
        let parts: Vec<&str> = normalized.split('-').collect();
        if parts.len() == 2 {
            Some(Self::new(parts[0], parts[1]))
        } else {
            None
        }
    }

    /// Convert to OCI platform format (os/arch).
    #[must_use]
    pub fn to_oci_platform(&self) -> String {
        let arch = match self.arch.as_str() {
            "x86_64" => "amd64",
            "arm64" => "arm64",
            other => other,
        };
        format!("{}/{}", self.os, arch)
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.os, self.arch)
    }
}

/// Get the current platform.
#[must_use]
pub fn current_platform() -> Platform {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    };
    Platform::new(os, arch)
}

/// Normalize a platform string to canonical format.
///
/// Handles various platform representations:
/// - "macos-amd64" -> "darwin-x86_64"
/// - "linux-aarch64" -> "linux-arm64"
/// - "Darwin-ARM64" -> "darwin-arm64"
#[must_use]
pub fn normalize_platform(platform: &str) -> String {
    let platform = platform.to_lowercase();

    platform
        .replace("macos", "darwin")
        .replace("osx", "darwin")
        .replace("amd64", "x86_64")
        .replace("aarch64", "arm64")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_parse() {
        let p = Platform::parse("darwin-arm64").unwrap();
        assert_eq!(p.os, "darwin");
        assert_eq!(p.arch, "arm64");
    }

    #[test]
    fn test_platform_to_oci() {
        let p = Platform::new("darwin", "arm64");
        assert_eq!(p.to_oci_platform(), "darwin/arm64");

        let p = Platform::new("linux", "x86_64");
        assert_eq!(p.to_oci_platform(), "linux/amd64");
    }

    #[test]
    fn test_normalize_platform() {
        assert_eq!(normalize_platform("macos-amd64"), "darwin-x86_64");
        assert_eq!(normalize_platform("linux-aarch64"), "linux-arm64");
        assert_eq!(normalize_platform("Darwin-ARM64"), "darwin-arm64");
    }

    #[test]
    fn test_current_platform() {
        let p = current_platform();
        assert!(!p.os.is_empty());
        assert!(!p.arch.is_empty());
    }
}
