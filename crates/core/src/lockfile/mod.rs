//! Lockfile types for OCI binary provider.
//!
//! The lockfile (`cuenv.lock`) stores resolved OCI artifact digests for
//! reproducible, hermetic builds. It aggregates all binaries from all
//! projects in a CUE module.
//!
//! ## Structure
//!
//! ```toml
//! version = 1
//!
//! [[artifacts]]
//! kind = "registry"
//! registry = "ghcr.io/homebrew/core"
//! name = "jq"
//! version = "1.7.1"
//!
//!   [artifacts.platforms]
//!   "darwin-arm64" = { digest = "sha256:abc...", size = 1234567 }
//!   "linux-x86_64" = { digest = "sha256:def...", size = 1345678 }
//!
//! [[artifacts]]
//! kind = "image"
//! image = "nginx:1.25-alpine"
//!
//!   [artifacts.platforms]
//!   "linux-amd64" = { digest = "sha256:ghi...", size = 9876543 }
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Current lockfile format version.
pub const LOCKFILE_VERSION: u32 = 1;

/// Filename for the lockfile.
pub const LOCKFILE_NAME: &str = "cuenv.lock";

/// The root lockfile structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lockfile {
    /// Lockfile format version (for future migrations).
    pub version: u32,
    /// All resolved OCI artifacts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<LockedArtifact>,
}

impl Default for Lockfile {
    fn default() -> Self {
        Self {
            version: LOCKFILE_VERSION,
            artifacts: Vec::new(),
        }
    }
}

impl Lockfile {
    /// Create a new empty lockfile.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a lockfile from a TOML file.
    ///
    /// Returns `None` if the file doesn't exist.
    /// Returns an error if the file exists but is invalid.
    pub fn load(path: &Path) -> crate::Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(path).map_err(|e| {
            crate::Error::configuration(format!("Failed to read lockfile: {}", e))
        })?;

        let lockfile: Self = toml::from_str(&content).map_err(|e| {
            crate::Error::configuration(format!("Failed to parse lockfile: {}", e))
        })?;

        // Version check for future migrations
        if lockfile.version > LOCKFILE_VERSION {
            return Err(crate::Error::configuration(format!(
                "Lockfile version {} is newer than supported version {}. Please upgrade cuenv.",
                lockfile.version, LOCKFILE_VERSION
            )));
        }

        Ok(Some(lockfile))
    }

    /// Save the lockfile to a TOML file.
    pub fn save(&self, path: &Path) -> crate::Result<()> {
        let content = toml::to_string_pretty(self).map_err(|e| {
            crate::Error::configuration(format!("Failed to serialize lockfile: {}", e))
        })?;

        std::fs::write(path, content).map_err(|e| {
            crate::Error::configuration(format!("Failed to write lockfile: {}", e))
        })?;

        Ok(())
    }

    /// Find an image artifact by image reference.
    #[must_use]
    pub fn find_image_artifact(&self, image: &str) -> Option<&LockedArtifact> {
        self.artifacts.iter().find(|a| {
            matches!(&a.kind, ArtifactKind::Image { image: img } if img == image)
        })
    }

    /// Add or update an artifact in the lockfile.
    pub fn upsert_artifact(&mut self, artifact: LockedArtifact) {
        // Find existing artifact with same kind
        let existing_idx = self.artifacts.iter().position(|a| a.kind == artifact.kind);

        if let Some(idx) = existing_idx {
            self.artifacts[idx] = artifact;
        } else {
            self.artifacts.push(artifact);
        }
    }
}

/// A locked OCI artifact with platform-specific digests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedArtifact {
    /// The kind of artifact (registry package or container image).
    #[serde(flatten)]
    pub kind: ArtifactKind,
    /// Platform-specific resolution data.
    /// Keys are platform strings like "darwin-arm64", "linux-x86_64".
    pub platforms: HashMap<String, PlatformData>,
}

impl LockedArtifact {
    /// Get the digest for the current platform.
    #[must_use]
    pub fn digest_for_current_platform(&self) -> Option<&str> {
        let platform = current_platform();
        self.platforms.get(&platform).map(|p| p.digest.as_str())
    }

    /// Get platform data for the current platform.
    #[must_use]
    pub fn platform_data(&self) -> Option<&PlatformData> {
        let platform = current_platform();
        self.platforms.get(&platform)
    }
}

/// The kind of OCI artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ArtifactKind {
    /// An OCI image (Homebrew bottles or container images).
    Image {
        /// Full image reference (e.g., "ghcr.io/homebrew/core/jq:1.7.1", "nginx:1.25-alpine").
        image: String,
    },
}

/// Platform-specific artifact data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlatformData {
    /// Content-addressable digest (e.g., "sha256:abc123...").
    pub digest: String,
    /// Size in bytes (for progress reporting).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// Get the current platform string.
///
/// Format: `{os}-{arch}` where:
/// - os: "darwin", "linux", "windows"
/// - arch: "x86_64", "arm64", "aarch64"
#[must_use]
pub fn current_platform() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    // Normalize arch names
    let arch = match arch {
        "aarch64" => "arm64",
        other => other,
    };

    format!("{}-{}", os, arch)
}

/// Normalize a platform string to our canonical format.
#[must_use]
pub fn normalize_platform(platform: &str) -> String {
    let platform = platform.to_lowercase();

    // Handle various platform formats
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
    fn test_lockfile_serialization() {
        let mut lockfile = Lockfile::new();

        lockfile.artifacts.push(LockedArtifact {
            kind: ArtifactKind::Image {
                image: "ghcr.io/homebrew/core/jq:1.7.1".to_string(),
            },
            platforms: HashMap::from([
                (
                    "darwin-arm64".to_string(),
                    PlatformData {
                        digest: "sha256:abc123".to_string(),
                        size: Some(1234567),
                    },
                ),
                (
                    "linux-x86_64".to_string(),
                    PlatformData {
                        digest: "sha256:def456".to_string(),
                        size: Some(1345678),
                    },
                ),
            ]),
        });

        lockfile.artifacts.push(LockedArtifact {
            kind: ArtifactKind::Image {
                image: "nginx:1.25-alpine".to_string(),
            },
            platforms: HashMap::from([(
                "linux-x86_64".to_string(),
                PlatformData {
                    digest: "sha256:ghi789".to_string(),
                    size: Some(9876543),
                },
            )]),
        });

        let toml_str = toml::to_string_pretty(&lockfile).unwrap();
        assert!(toml_str.contains("version = 1"));
        assert!(toml_str.contains("ghcr.io/homebrew/core/jq:1.7.1"));
        assert!(toml_str.contains("nginx:1.25-alpine"));

        // Round-trip test
        let parsed: Lockfile = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed, lockfile);
    }

    #[test]
    fn test_find_image_artifact() {
        let mut lockfile = Lockfile::new();
        lockfile.artifacts.push(LockedArtifact {
            kind: ArtifactKind::Image {
                image: "nginx:1.25-alpine".to_string(),
            },
            platforms: HashMap::new(),
        });

        assert!(lockfile.find_image_artifact("nginx:1.25-alpine").is_some());
        assert!(lockfile.find_image_artifact("nginx:1.24-alpine").is_none());
    }

    #[test]
    fn test_upsert_artifact() {
        let mut lockfile = Lockfile::new();

        let artifact1 = LockedArtifact {
            kind: ArtifactKind::Image {
                image: "ghcr.io/homebrew/core/jq:1.7.1".to_string(),
            },
            platforms: HashMap::from([(
                "darwin-arm64".to_string(),
                PlatformData {
                    digest: "sha256:old".to_string(),
                    size: None,
                },
            )]),
        };

        lockfile.upsert_artifact(artifact1);
        assert_eq!(lockfile.artifacts.len(), 1);

        // Update with new digest
        let artifact2 = LockedArtifact {
            kind: ArtifactKind::Image {
                image: "ghcr.io/homebrew/core/jq:1.7.1".to_string(),
            },
            platforms: HashMap::from([(
                "darwin-arm64".to_string(),
                PlatformData {
                    digest: "sha256:new".to_string(),
                    size: Some(123),
                },
            )]),
        };

        lockfile.upsert_artifact(artifact2);
        assert_eq!(lockfile.artifacts.len(), 1);
        assert_eq!(
            lockfile.artifacts[0].platforms["darwin-arm64"].digest,
            "sha256:new"
        );
    }

    #[test]
    fn test_current_platform() {
        let platform = current_platform();
        // Should contain OS and arch
        assert!(platform.contains('-'));
        let parts: Vec<&str> = platform.split('-').collect();
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn test_normalize_platform() {
        assert_eq!(normalize_platform("macos-amd64"), "darwin-x86_64");
        assert_eq!(normalize_platform("linux-aarch64"), "linux-arm64");
        assert_eq!(normalize_platform("Darwin-ARM64"), "darwin-arm64");
    }
}
