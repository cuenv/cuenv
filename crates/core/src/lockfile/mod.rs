//! Lockfile types for cuenv tool management.
//!
//! The lockfile (`cuenv.lock`) stores resolved runtime and tool digests for
//! reproducible, hermetic builds. It supports multiple sources:
//! Nix flakes, GitHub releases, and OCI images.
//!
//! ## Structure (v3)
//!
//! ```toml
//! version = 4
//!
//! # Runtime section - project runtime state
//! [runtimes."."]
//! type = "nix"
//! flake = "."
//! digest = "sha256:runtime..."
//! lockfile = "flake.lock"
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
//! [[tools_activation]]
//! var = "PATH"
//! op = "prepend"
//! separator = ":"
//! from = { type = "allBinDirs" }
//!
//! # Legacy artifacts section (for OCI images)
//! [[artifacts]]
//! kind = "image"
//! image = "nginx:1.25-alpine"
//!
//!   [artifacts.platforms]
//!   "linux-x86_64" = { digest = "sha256:abc...", size = 1234567 }
//! ```

use crate::tools::ToolActivationStep;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Component, Path};

/// Current lockfile format version.
pub const LOCKFILE_VERSION: u32 = 4;

/// Filename for the lockfile.
pub const LOCKFILE_NAME: &str = "cuenv.lock";

/// The root lockfile structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lockfile {
    /// Lockfile format version (for future migrations).
    pub version: u32,
    /// Locked project runtimes keyed by project path.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub runtimes: BTreeMap<String, LockedRuntime>,
    /// Locked tools with per-platform resolution (v2+).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, LockedTool>,
    /// Tool activation operations shared by CLI/CI execution paths (v3+).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_activation: Vec<ToolActivationStep>,
    /// Locked VCS dependencies (v4+).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vcs: BTreeMap<String, LockedVcsDependency>,
    /// Legacy OCI artifacts (for backward compatibility with v1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<LockedArtifact>,
}

impl Default for Lockfile {
    fn default() -> Self {
        Self {
            version: LOCKFILE_VERSION,
            runtimes: BTreeMap::new(),
            tools: BTreeMap::new(),
            tools_activation: Vec::new(),
            vcs: BTreeMap::new(),
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
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(crate::Error::configuration(format!(
                    "Refusing to read symlinked lockfile at {}",
                    path.display()
                )));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(crate::Error::configuration(format!(
                    "Failed to inspect lockfile at {}: {}",
                    path.display(),
                    e
                )));
            }
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
        lockfile.validate().map_err(|msg| {
            crate::Error::configuration(format!("Invalid lockfile at {}: {}", path.display(), msg))
        })?;

        Ok(Some(lockfile))
    }

    /// Save the lockfile to a TOML file.
    pub fn save(&self, path: &Path) -> crate::Result<()> {
        self.validate().map_err(|msg| {
            crate::Error::configuration(format!("Refusing to write invalid lockfile: {msg}"))
        })?;

        let content = toml::to_string_pretty(self).map_err(|e| {
            crate::Error::configuration(format!("Failed to serialize lockfile: {}", e))
        })?;

        if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(crate::Error::configuration(format!(
                "Refusing to write symlinked lockfile at {}",
                path.display()
            )));
        }

        let temp_path = path.with_file_name(format!(
            ".{}.tmp-{}-{}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("cuenv.lock"),
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| crate::Error::configuration(e.to_string()))?
                .as_nanos()
        ));
        let mut temp_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|e| crate::Error::configuration(format!("Failed to write lockfile: {}", e)))?;
        temp_file
            .write_all(content.as_bytes())
            .map_err(|e| crate::Error::configuration(format!("Failed to write lockfile: {}", e)))?;
        temp_file
            .sync_all()
            .map_err(|e| crate::Error::configuration(format!("Failed to sync lockfile: {}", e)))?;
        drop(temp_file);
        std::fs::rename(&temp_path, path).map_err(|e| {
            let _ = std::fs::remove_file(&temp_path);
            crate::Error::configuration(format!("Failed to replace lockfile: {}", e))
        })?;
        let parent = lockfile_parent_for_sync(path);
        std::fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| {
                crate::Error::configuration(format!("Failed to sync lockfile directory: {}", e))
            })?;

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

    /// Find a locked runtime by project path.
    #[must_use]
    pub fn find_runtime(&self, project_path: &str) -> Option<&LockedRuntime> {
        self.runtimes.get(project_path)
    }

    /// Find a locked VCS dependency by name.
    #[must_use]
    pub fn find_vcs(&self, name: &str) -> Option<&LockedVcsDependency> {
        self.vcs.get(name)
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

    /// Add or update a runtime in the lockfile.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime fails validation.
    pub fn upsert_runtime(
        &mut self,
        project_path: String,
        runtime: LockedRuntime,
    ) -> crate::Result<()> {
        runtime.validate().map_err(|msg| {
            crate::Error::configuration(format!(
                "Invalid runtime for project '{}': {}",
                project_path, msg
            ))
        })?;

        self.runtimes.insert(project_path, runtime);
        Ok(())
    }

    /// Add or update a VCS dependency in the lockfile.
    ///
    /// # Errors
    ///
    /// Returns an error if the dependency fails validation.
    pub fn upsert_vcs(
        &mut self,
        name: String,
        dependency: LockedVcsDependency,
    ) -> crate::Result<()> {
        dependency.validate().map_err(|msg| {
            crate::Error::configuration(format!("Invalid VCS dependency '{}': {}", name, msg))
        })?;

        self.vcs.insert(name, dependency);
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

    fn validate(&self) -> Result<(), String> {
        for (name, dependency) in &self.vcs {
            dependency
                .validate()
                .map_err(|msg| format!("invalid VCS dependency '{}': {}", name, msg))?;
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

/// A locked project runtime keyed by project path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LockedRuntime {
    /// Locked Nix runtime derived from a local flake.lock file.
    Nix(LockedNixRuntime),
}

impl LockedRuntime {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Nix(runtime) => runtime.validate(),
        }
    }
}

/// Locked metadata for a Nix runtime.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedNixRuntime {
    /// Flake reference from the manifest runtime.
    pub flake: String,
    /// Selected shell or package output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Deterministic digest derived from the local flake.lock content.
    pub digest: String,
    /// Relative path to the flake.lock file used for this digest.
    pub lockfile: String,
}

impl LockedNixRuntime {
    fn validate(&self) -> Result<(), String> {
        if !self.digest.starts_with("sha256:") && !self.digest.starts_with("sha512:") {
            return Err(
                "digest must start with 'sha256:' or 'sha512:' for Nix runtime".to_string(),
            );
        }

        if self.lockfile.trim().is_empty() {
            return Err("lockfile path must not be empty".to_string());
        }

        Ok(())
    }
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

/// A locked cuenv-managed VCS dependency.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedVcsDependency {
    /// Git repository URL.
    pub url: String,
    /// Requested branch, tag, or commit-ish.
    pub reference: String,
    /// Resolved commit SHA.
    pub commit: String,
    /// Tree object SHA for the resolved commit (repository root).
    pub tree: String,
    /// Whether the materialized dependency is a tracked source snapshot.
    pub vendor: bool,
    /// Repository-relative materialization path.
    pub path: String,
    /// Subdirectory of the repo that was vendored (sparse checkout).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
    /// Tree object SHA for the vendored subdirectory at `commit:subdir`.
    /// Only populated when `subdir` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtree: Option<String>,
}

impl LockedVcsDependency {
    fn validate(&self) -> Result<(), String> {
        if self.url.trim().is_empty() {
            return Err("url must not be empty".to_string());
        }
        if self.reference.trim().is_empty() {
            return Err("reference must not be empty".to_string());
        }
        if !is_git_object_id(&self.commit) {
            return Err("commit must be a hexadecimal Git object ID".to_string());
        }
        if !is_git_object_id(&self.tree) {
            return Err("tree must be a hexadecimal Git object ID".to_string());
        }
        if self.path.trim().is_empty() {
            return Err("path must not be empty".to_string());
        }
        validate_locked_vcs_path(&self.path)?;
        match (self.subdir.as_deref(), self.subtree.as_deref()) {
            (None, None) => {}
            (Some(subdir), Some(subtree)) => {
                if subdir.trim().is_empty() {
                    return Err("subdir must not be empty".to_string());
                }
                validate_locked_vcs_subdir(subdir)?;
                if !is_git_object_id(subtree) {
                    return Err("subtree must be a hexadecimal Git object ID".to_string());
                }
                if !self.vendor {
                    return Err("subdir requires vendor = true".to_string());
                }
            }
            _ => return Err("subdir and subtree must be set together".to_string()),
        }
        Ok(())
    }
}

fn is_git_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn validate_locked_vcs_path(path: &str) -> Result<(), String> {
    let components = parse_locked_relative_components(path)?;
    if components.iter().any(|component| component == ".git")
        || components.starts_with(&[".cuenv".to_string(), "vcs".to_string(), "cache".to_string()])
        || components.starts_with(&[".cuenv".to_string(), "vcs".to_string(), "tmp".to_string()])
    {
        return Err("path targets cuenv or git internals".to_string());
    }
    Ok(())
}

/// Validate a sparse-checkout subdir as recorded in the lockfile.
///
/// Like [`validate_locked_vcs_path`] but without the local-disk reserved-path
/// checks: `subdir` is a path *inside the remote repository*, so a remote repo
/// that happens to contain `.cuenv/...` is a legitimate target. We still
/// reject `.git` components, since git itself does not allow them as tracked
/// directories.
fn validate_locked_vcs_subdir(subdir: &str) -> Result<(), String> {
    let components = parse_locked_relative_components(subdir)?;
    if components.iter().any(|component| component == ".git") {
        return Err("subdir must not contain a '.git' component".to_string());
    }
    Ok(())
}

fn parse_locked_relative_components(path: &str) -> Result<Vec<String>, String> {
    let rel = Path::new(path);
    if rel.is_absolute() || path.trim().is_empty() {
        return Err("path must be relative".to_string());
    }
    let mut components = Vec::new();
    for component in rel.components() {
        let Component::Normal(value) = component else {
            return Err("path must not contain '.', '..', or prefixes".to_string());
        };
        let value = value.to_string_lossy();
        if value.is_empty()
            || value == "."
            || value == ".."
            || value.starts_with('-')
            || value.contains('\\')
            || value.chars().any(|c| {
                c.is_control()
                    || matches!(
                        c,
                        '*' | '?' | '[' | ']' | '!' | '#' | ' ' | '\t' | '\n' | '\r'
                    )
            })
        {
            return Err("path contains unsafe components".to_string());
        }
        components.push(value.into_owned());
    }
    if components.is_empty() {
        return Err("path must not target the repository root".to_string());
    }
    Ok(components)
}

fn lockfile_parent_for_sync(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
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

        lockfile
            .upsert_runtime(
                ".".to_string(),
                LockedRuntime::Nix(LockedNixRuntime {
                    flake: ".".to_string(),
                    output: None,
                    digest: "sha256:runtime123".to_string(),
                    lockfile: "flake.lock".to_string(),
                }),
            )
            .unwrap();

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
        assert!(toml_str.contains("version = 4"));
        assert!(toml_str.contains("type = \"nix\""));
        assert!(toml_str.contains("lockfile = \"flake.lock\""));
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
        assert!(toml_str.contains("version = 4"));
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
    fn test_tools_activation_serialization() {
        use crate::tools::{ToolActivationOperation, ToolActivationSource, ToolActivationStep};

        let mut lockfile = Lockfile::new();
        lockfile.tools_activation.push(ToolActivationStep {
            var: "PATH".to_string(),
            op: ToolActivationOperation::Prepend,
            separator: ":".to_string(),
            from: ToolActivationSource::AllBinDirs,
        });

        let toml_str = toml::to_string_pretty(&lockfile).unwrap();
        assert!(toml_str.contains("[[tools_activation]]"));
        assert!(toml_str.contains("var = \"PATH\""));
        assert!(toml_str.contains("op = \"prepend\""));
        assert!(toml_str.contains("type = \"allBinDirs\""));

        let parsed: Lockfile = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.tools_activation.len(), 1);
        assert_eq!(parsed.tools_activation[0], lockfile.tools_activation[0]);
    }

    #[test]
    fn test_find_runtime() {
        let mut lockfile = Lockfile::new();
        lockfile
            .upsert_runtime(
                ".".to_string(),
                LockedRuntime::Nix(LockedNixRuntime {
                    flake: ".".to_string(),
                    output: Some("devShells.x86_64-linux.default".to_string()),
                    digest: "sha256:abc123".to_string(),
                    lockfile: "flake.lock".to_string(),
                }),
            )
            .unwrap();

        assert!(lockfile.find_runtime(".").is_some());
        assert!(lockfile.find_runtime("apps/api").is_none());
    }

    #[test]
    fn test_vcs_serialization() {
        let mut lockfile = Lockfile::new();
        lockfile
            .upsert_vcs(
                "mylib".to_string(),
                LockedVcsDependency {
                    url: "https://github.com/example/mylib.git".to_string(),
                    reference: "main".to_string(),
                    commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
                    tree: "89abcdef012345670123456789abcdef01234567".to_string(),
                    vendor: true,
                    path: "vendor/mylib".to_string(),
                    subdir: None,
                    subtree: None,
                },
            )
            .unwrap();

        let toml_str = toml::to_string_pretty(&lockfile).unwrap();
        assert!(toml_str.contains("version = 4"));
        assert!(toml_str.contains("[vcs.mylib]"));
        assert!(toml_str.contains("reference = \"main\""));

        let parsed: Lockfile = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.find_vcs("mylib").unwrap().path, "vendor/mylib");
    }

    #[test]
    fn test_vcs_subdir_serialization_roundtrip() {
        let mut lockfile = Lockfile::new();
        lockfile
            .upsert_vcs(
                "skills".to_string(),
                LockedVcsDependency {
                    url: "https://github.com/cuenv/cuenv.git".to_string(),
                    reference: "0.27.1".to_string(),
                    commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
                    tree: "89abcdef012345670123456789abcdef01234567".to_string(),
                    vendor: true,
                    path: ".agents/skills".to_string(),
                    subdir: Some(".agents/skills".to_string()),
                    subtree: Some("ffffffffffffffffffffffffffffffffffffffff".to_string()),
                },
            )
            .unwrap();

        let toml_str = toml::to_string_pretty(&lockfile).unwrap();
        assert!(toml_str.contains("subdir = \".agents/skills\""));
        assert!(toml_str.contains("subtree = \"ffffffffffffffffffffffffffffffffffffffff\""));

        let parsed: Lockfile = toml::from_str(&toml_str).unwrap();
        let dep = parsed.find_vcs("skills").unwrap();
        assert_eq!(dep.subdir.as_deref(), Some(".agents/skills"));
        assert_eq!(
            dep.subtree.as_deref(),
            Some("ffffffffffffffffffffffffffffffffffffffff")
        );
    }

    #[test]
    fn test_vcs_without_subdir_omits_fields() {
        let mut lockfile = Lockfile::new();
        lockfile
            .upsert_vcs(
                "plain".to_string(),
                LockedVcsDependency {
                    url: "https://github.com/example/plain.git".to_string(),
                    reference: "main".to_string(),
                    commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
                    tree: "89abcdef012345670123456789abcdef01234567".to_string(),
                    vendor: true,
                    path: "vendor/plain".to_string(),
                    subdir: None,
                    subtree: None,
                },
            )
            .unwrap();

        let toml_str = toml::to_string_pretty(&lockfile).unwrap();
        assert!(!toml_str.contains("subdir"));
        assert!(!toml_str.contains("subtree"));

        let parsed: Lockfile = toml::from_str(&toml_str).unwrap();
        let dep = parsed.find_vcs("plain").unwrap();
        assert_eq!(dep.subdir, None);
        assert_eq!(dep.subtree, None);
    }

    #[test]
    fn test_legacy_vcs_entry_without_subdir_loads() {
        // A lockfile written before subdir/subtree existed should still load.
        let legacy = r#"
version = 4

[vcs.legacy]
url = "https://github.com/example/legacy.git"
reference = "main"
commit = "0123456789abcdef0123456789abcdef01234567"
tree = "89abcdef012345670123456789abcdef01234567"
vendor = true
path = "vendor/legacy"
"#;
        let parsed: Lockfile = toml::from_str(legacy).expect("legacy lockfile parses");
        let dep = parsed.find_vcs("legacy").expect("entry present");
        assert_eq!(dep.subdir, None);
        assert_eq!(dep.subtree, None);
    }

    #[test]
    fn test_lockfile_parent_for_sync_handles_relative_path() {
        assert_eq!(
            lockfile_parent_for_sync(Path::new("cuenv.lock")),
            Path::new(".")
        );
        assert_eq!(
            lockfile_parent_for_sync(Path::new("nested/cuenv.lock")),
            Path::new("nested")
        );
    }

    #[test]
    fn test_vcs_path_rejects_internal_paths() {
        assert!(validate_locked_vcs_path(".git/hooks").is_err());
        assert!(validate_locked_vcs_path("vendor/.git/hooks").is_err());
        assert!(validate_locked_vcs_path(".cuenv/vcs/cache/lib").is_err());
        assert!(validate_locked_vcs_path(".cuenv/vcs/tmp/lib").is_err());
        assert!(validate_locked_vcs_path("vendor/lib").is_ok());
    }

    #[test]
    fn test_vcs_subdir_allows_dotcuenv_paths_but_rejects_dotgit() {
        // subdir is a path *inside the remote repo*, so .cuenv/... is allowed —
        // only local-disk materialization paths reserve those prefixes.
        assert!(validate_locked_vcs_subdir(".cuenv/vcs/cache").is_ok());
        assert!(validate_locked_vcs_subdir(".cuenv/some/skill").is_ok());
        assert!(validate_locked_vcs_subdir(".agents/skills").is_ok());

        // .git inside a tree is still impossible under git's own rules.
        assert!(validate_locked_vcs_subdir(".git").is_err());
        assert!(validate_locked_vcs_subdir("nested/.git").is_err());

        // Component-safety rules still apply.
        assert!(validate_locked_vcs_subdir("--stdin").is_err());
        assert!(validate_locked_vcs_subdir("nested/-evil").is_err());
        assert!(validate_locked_vcs_subdir("a\\b").is_err());
        assert!(validate_locked_vcs_subdir("..").is_err());
        assert!(validate_locked_vcs_subdir("").is_err());
    }

    #[test]
    fn test_upsert_runtime_validation_invalid_digest() {
        let mut lockfile = Lockfile::new();

        let result = lockfile.upsert_runtime(
            ".".to_string(),
            LockedRuntime::Nix(LockedNixRuntime {
                flake: ".".to_string(),
                output: None,
                digest: "invalid".to_string(),
                lockfile: "flake.lock".to_string(),
            }),
        );

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("digest must start with")
        );
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
