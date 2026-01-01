//! Lockfile types for cuenv tool management.
//!
//! The lockfile (`cuenv.lock`) stores resolved tool digests for
//! reproducible, hermetic builds. It supports multiple tool sources:
//! GitHub releases, Nix flakes, and OCI images.
//!
//! ## Structure (v2)
//!
//! ```toml
//! version = 2
//!
//! # Tools section - multi-source tool management
//! [tools.jq]
//! version = "1.7.1"
//!
//!   [tools.jq.platforms."darwin-arm64"]
//!   provider = "github"
//!   digest = "sha256:abc..."
//!   source = { repo = "jqlang/jq", tag = "jq-1.7.1", asset = "jq-macos-arm64" }
//!
//!   [tools.jq.platforms."linux-x86_64"]
//!   provider = "github"
//!   digest = "sha256:def..."
//!   source = { repo = "jqlang/jq", tag = "jq-1.7.1", asset = "jq-linux-amd64" }
//!
//! [tools.rust]
//! version = "1.83.0"
//!
//!   [tools.rust.platforms."darwin-arm64"]
//!   provider = "nix"
//!   digest = "sha256:ghi..."
//!   source = { flake = "nixpkgs", package = "rustc" }
//!
//! # Legacy artifacts section (for OCI images)
//! [[artifacts]]
//! kind = "image"
//! image = "nginx:1.25-alpine"
//!
//!   [artifacts.platforms]
//!   "linux-x86_64" = { digest = "sha256:abc...", size = 1234567 }
//! ```

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Current lockfile format version.
pub const LOCKFILE_VERSION: u32 = 2;

/// Filename for the lockfile.
pub const LOCKFILE_NAME: &str = "cuenv.lock";

/// The root lockfile structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lockfile {
    /// Lockfile format version (for future migrations).
    pub version: u32,
    /// Locked tools with per-platform resolution (v2+).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, LockedTool>,
    /// Legacy OCI artifacts (for backward compatibility with v1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<LockedArtifact>,
}

impl Default for Lockfile {
    fn default() -> Self {
        Self {
            version: LOCKFILE_VERSION,
            tools: BTreeMap::new(),
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

        let content = std::fs::read_to_string(path)
            .map_err(|e| crate::Error::configuration(format!("Failed to read lockfile: {}", e)))?;

        let lockfile: Self = toml::from_str(&content).map_err(|e| {
            crate::Error::configuration(format!(
                "Failed to parse lockfile at {}: {}",
                path.display(),
                e
            ))
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

        std::fs::write(path, content)
            .map_err(|e| crate::Error::configuration(format!("Failed to write lockfile: {}", e)))?;

        Ok(())
    }

    /// Find an image artifact by image reference.
    #[must_use]
    pub fn find_image_artifact(&self, image: &str) -> Option<&LockedArtifact> {
        self.artifacts
            .iter()
            .find(|a| matches!(&a.kind, ArtifactKind::Image { image: img } if img == image))
    }

    /// Find a tool by name.
    #[must_use]
    pub fn find_tool(&self, name: &str) -> Option<&LockedTool> {
        self.tools.get(name)
    }

    /// Get all tool names.
    #[must_use]
    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.keys().map(String::as_str).collect()
    }

    /// Add or update a tool in the lockfile.
    ///
    /// # Errors
    ///
    /// Returns an error if the tool fails validation (empty platforms or
    /// invalid digest format).
    pub fn upsert_tool(&mut self, name: String, tool: LockedTool) -> crate::Result<()> {
        tool.validate().map_err(|msg| {
            crate::Error::configuration(format!("Invalid tool '{}': {}", name, msg))
        })?;

        self.tools.insert(name, tool);
        Ok(())
    }

    /// Add or update a single platform for a tool.
    ///
    /// Creates the tool entry if it doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the platform data is invalid.
    pub fn upsert_tool_platform(
        &mut self,
        name: &str,
        version: &str,
        platform: &str,
        data: LockedToolPlatform,
    ) -> crate::Result<()> {
        // Validate digest format
        if !data.digest.starts_with("sha256:") && !data.digest.starts_with("sha512:") {
            return Err(crate::Error::configuration(format!(
                "Invalid digest format for tool '{}' platform '{}': must start with 'sha256:' or 'sha512:'",
                name, platform
            )));
        }

        let tool = self
            .tools
            .entry(name.to_string())
            .or_insert_with(|| LockedTool {
                version: version.to_string(),
                platforms: BTreeMap::new(),
            });

        // Update version if it changed
        if tool.version != version {
            tool.version = version.to_string();
        }

        tool.platforms.insert(platform.to_string(), data);
        Ok(())
    }

    /// Add or update an artifact in the lockfile (legacy v1 format).
    ///
    /// Matches artifacts by image reference.
    ///
    /// # Errors
    ///
    /// Returns an error if the artifact fails validation (empty platforms or
    /// invalid digest format).
    pub fn upsert_artifact(&mut self, artifact: LockedArtifact) -> crate::Result<()> {
        // Validate the artifact before inserting
        artifact
            .validate()
            .map_err(|msg| crate::Error::configuration(format!("Invalid artifact: {}", msg)))?;

        // Find existing artifact with same identity (match Image by full reference)
        let existing_idx = self
            .artifacts
            .iter()
            .position(|a| match (&a.kind, &artifact.kind) {
                (ArtifactKind::Image { image: i1 }, ArtifactKind::Image { image: i2 }) => i1 == i2,
            });

        if let Some(idx) = existing_idx {
            self.artifacts[idx] = artifact;
        } else {
            self.artifacts.push(artifact);
        }

        Ok(())
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
    pub platforms: BTreeMap<String, PlatformData>,
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

    /// Validate the artifact has valid data.
    ///
    /// Checks:
    /// - At least one platform is present
    /// - All digests have valid format (sha256: or sha512: prefix)
    fn validate(&self) -> Result<(), String> {
        if self.platforms.is_empty() {
            return Err("Artifact must have at least one platform".to_string());
        }

        for (platform, data) in &self.platforms {
            if !data.digest.starts_with("sha256:") && !data.digest.starts_with("sha512:") {
                return Err(format!(
                    "Invalid digest format for platform '{}': must start with 'sha256:' or 'sha512:'",
                    platform
                ));
            }
        }

        Ok(())
    }
}

/// The kind of OCI artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ArtifactKind {
    /// An OCI image (container images).
    Image {
        /// Full image reference (e.g., "nginx:1.25-alpine").
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

/// A locked tool with version and per-platform resolution (v2+).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedTool {
    /// Version string (e.g., "1.7.1").
    pub version: String,
    /// Platform-specific resolution data.
    /// Keys are platform strings like "darwin-arm64", "linux-x86_64".
    pub platforms: BTreeMap<String, LockedToolPlatform>,
}

impl LockedTool {
    /// Get platform data for the current platform.
    #[must_use]
    pub fn current_platform(&self) -> Option<&LockedToolPlatform> {
        let platform = current_platform();
        self.platforms.get(&platform)
    }

    /// Validate the locked tool has valid data.
    fn validate(&self) -> Result<(), String> {
        if self.platforms.is_empty() {
            return Err("Tool must have at least one platform".to_string());
        }

        for (platform, data) in &self.platforms {
            if !data.digest.starts_with("sha256:") && !data.digest.starts_with("sha512:") {
                return Err(format!(
                    "Invalid digest format for platform '{}': must start with 'sha256:' or 'sha512:'",
                    platform
                ));
            }
        }

        Ok(())
    }
}

/// Platform-specific tool resolution data (v2+).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedToolPlatform {
    /// Provider that resolved this tool (e.g., "github", "nix", "oci").
    pub provider: String,
    /// Content-addressable digest (e.g., "sha256:abc123...").
    pub digest: String,
    /// Provider-specific source data (serialized as inline table).
    /// For GitHub: `{ repo = "...", tag = "...", asset = "..." }`
    /// For Nix: `{ flake = "...", package = "..." }`
    /// For OCI: `{ image = "...", path = "..." }`
    pub source: serde_json::Value,
    /// Size in bytes (for progress reporting).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Runtime dependencies (other tool names).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
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

        // OCI image artifact
        lockfile.artifacts.push(LockedArtifact {
            kind: ArtifactKind::Image {
                image: "nginx:1.25-alpine".to_string(),
            },
            platforms: BTreeMap::from([
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

        let toml_str = toml::to_string_pretty(&lockfile).unwrap();
        assert!(toml_str.contains("version = 2"));
        assert!(toml_str.contains("kind = \"image\""));
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
            platforms: BTreeMap::new(),
        });

        assert!(lockfile.find_image_artifact("nginx:1.25-alpine").is_some());
        assert!(lockfile.find_image_artifact("nginx:1.24-alpine").is_none());
    }

    #[test]
    fn test_upsert_artifact() {
        let mut lockfile = Lockfile::new();

        let artifact1 = LockedArtifact {
            kind: ArtifactKind::Image {
                image: "nginx:1.25-alpine".to_string(),
            },
            platforms: BTreeMap::from([(
                "darwin-arm64".to_string(),
                PlatformData {
                    digest: "sha256:old".to_string(),
                    size: None,
                },
            )]),
        };

        lockfile.upsert_artifact(artifact1).unwrap();
        assert_eq!(lockfile.artifacts.len(), 1);

        // Update with new digest
        let artifact2 = LockedArtifact {
            kind: ArtifactKind::Image {
                image: "nginx:1.25-alpine".to_string(),
            },
            platforms: BTreeMap::from([(
                "darwin-arm64".to_string(),
                PlatformData {
                    digest: "sha256:new".to_string(),
                    size: Some(123),
                },
            )]),
        };

        lockfile.upsert_artifact(artifact2).unwrap();
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

    #[test]
    fn test_upsert_artifact_validation_empty_platforms() {
        let mut lockfile = Lockfile::new();

        let artifact = LockedArtifact {
            kind: ArtifactKind::Image {
                image: "nginx:1.25-alpine".to_string(),
            },
            platforms: BTreeMap::new(), // Empty - should fail
        };

        let result = lockfile.upsert_artifact(artifact);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("at least one platform")
        );
    }

    #[test]
    fn test_upsert_artifact_validation_invalid_digest() {
        let mut lockfile = Lockfile::new();

        let artifact = LockedArtifact {
            kind: ArtifactKind::Image {
                image: "nginx:1.25-alpine".to_string(),
            },
            platforms: BTreeMap::from([(
                "darwin-arm64".to_string(),
                PlatformData {
                    digest: "invalid-no-prefix".to_string(), // Missing sha256: prefix
                    size: None,
                },
            )]),
        };

        let result = lockfile.upsert_artifact(artifact);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid digest format")
        );
    }

    #[test]
    fn test_artifact_validate_valid() {
        let artifact = LockedArtifact {
            kind: ArtifactKind::Image {
                image: "nginx:1.25-alpine".to_string(),
            },
            platforms: BTreeMap::from([
                (
                    "darwin-arm64".to_string(),
                    PlatformData {
                        digest: "sha256:abc123".to_string(),
                        size: Some(1234),
                    },
                ),
                (
                    "linux-x86_64".to_string(),
                    PlatformData {
                        digest: "sha512:def456".to_string(),
                        size: None,
                    },
                ),
            ]),
        };

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn test_tools_serialization() {
        let mut lockfile = Lockfile::new();

        lockfile
            .upsert_tool_platform(
                "jq",
                "1.7.1",
                "darwin-arm64",
                LockedToolPlatform {
                    provider: "github".to_string(),
                    digest: "sha256:abc123".to_string(),
                    source: serde_json::json!({ "repo": "jqlang/jq", "tag": "jq-1.7.1", "asset": "jq-macos-arm64" }),
                    size: Some(1234567),
                    dependencies: vec![],
                },
            )
            .unwrap();

        lockfile
            .upsert_tool_platform(
                "jq",
                "1.7.1",
                "linux-x86_64",
                LockedToolPlatform {
                    provider: "github".to_string(),
                    digest: "sha256:def456".to_string(),
                    source: serde_json::json!({ "repo": "jqlang/jq", "tag": "jq-1.7.1", "asset": "jq-linux-amd64" }),
                    size: Some(1345678),
                    dependencies: vec![],
                },
            )
            .unwrap();

        let toml_str = toml::to_string_pretty(&lockfile).unwrap();
        assert!(toml_str.contains("version = 2"));
        assert!(toml_str.contains("[tools.jq]"));
        assert!(toml_str.contains("provider = \"github\""));
        assert!(toml_str.contains("digest = \"sha256:abc123\""));

        // Round-trip test
        let parsed: Lockfile = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.tools.len(), 1);
        assert_eq!(parsed.tools["jq"].version, "1.7.1");
        assert_eq!(parsed.tools["jq"].platforms.len(), 2);
    }

    #[test]
    fn test_find_tool() {
        let mut lockfile = Lockfile::new();
        lockfile
            .upsert_tool_platform(
                "jq",
                "1.7.1",
                "darwin-arm64",
                LockedToolPlatform {
                    provider: "github".to_string(),
                    digest: "sha256:abc123".to_string(),
                    source: serde_json::json!({ "repo": "jqlang/jq", "tag": "jq-1.7.1", "asset": "jq-macos-arm64" }),
                    size: None,
                    dependencies: vec![],
                },
            )
            .unwrap();

        assert!(lockfile.find_tool("jq").is_some());
        assert!(lockfile.find_tool("yq").is_none());
    }

    #[test]
    fn test_upsert_tool_platform() {
        let mut lockfile = Lockfile::new();

        // Add first platform
        lockfile
            .upsert_tool_platform(
                "bun",
                "1.3.5",
                "darwin-arm64",
                LockedToolPlatform {
                    provider: "github".to_string(),
                    digest: "sha256:aaa".to_string(),
                    source: serde_json::json!({ "url": "https://..." }),
                    size: None,
                    dependencies: vec![],
                },
            )
            .unwrap();

        assert_eq!(lockfile.tools.len(), 1);
        assert_eq!(lockfile.tools["bun"].platforms.len(), 1);

        // Add second platform
        lockfile
            .upsert_tool_platform(
                "bun",
                "1.3.5",
                "linux-x86_64",
                LockedToolPlatform {
                    provider: "oci".to_string(),
                    digest: "sha256:bbb".to_string(),
                    source: serde_json::json!({ "image": "oven/bun:1.3.5" }),
                    size: None,
                    dependencies: vec![],
                },
            )
            .unwrap();

        assert_eq!(lockfile.tools.len(), 1);
        assert_eq!(lockfile.tools["bun"].platforms.len(), 2);
        assert_eq!(
            lockfile.tools["bun"].platforms["darwin-arm64"].provider,
            "github"
        );
        assert_eq!(
            lockfile.tools["bun"].platforms["linux-x86_64"].provider,
            "oci"
        );
    }

    #[test]
    fn test_upsert_tool_platform_invalid_digest() {
        let mut lockfile = Lockfile::new();

        let result = lockfile.upsert_tool_platform(
            "jq",
            "1.7.1",
            "darwin-arm64",
            LockedToolPlatform {
                provider: "github".to_string(),
                digest: "invalid".to_string(), // Missing sha256: prefix
                source: serde_json::json!({}),
                size: None,
                dependencies: vec![],
            },
        );

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid digest format")
        );
    }

    #[test]
    fn test_upsert_tool_validation_empty_platforms() {
        let mut lockfile = Lockfile::new();

        let tool = LockedTool {
            version: "1.7.1".to_string(),
            platforms: BTreeMap::new(), // Empty - should fail
        };

        let result = lockfile.upsert_tool("jq".to_string(), tool);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("at least one platform")
        );
    }

    #[test]
    fn test_tool_names() {
        let mut lockfile = Lockfile::new();

        lockfile
            .upsert_tool_platform(
                "jq",
                "1.7.1",
                "darwin-arm64",
                LockedToolPlatform {
                    provider: "github".to_string(),
                    digest: "sha256:abc".to_string(),
                    source: serde_json::json!({}),
                    size: None,
                    dependencies: vec![],
                },
            )
            .unwrap();

        lockfile
            .upsert_tool_platform(
                "yq",
                "4.44.6",
                "darwin-arm64",
                LockedToolPlatform {
                    provider: "github".to_string(),
                    digest: "sha256:def".to_string(),
                    source: serde_json::json!({}),
                    size: None,
                    dependencies: vec![],
                },
            )
            .unwrap();

        let names = lockfile.tool_names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"jq"));
        assert!(names.contains(&"yq"));
    }
}
