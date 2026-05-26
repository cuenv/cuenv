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
            .find(|a| matches!(&a.kind, ArtifactKind::Image { image: img, .. } if img == image))
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
                (ArtifactKind::Image { image: i1, .. }, ArtifactKind::Image { image: i2, .. }) => {
                    i1 == i2
                }
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
        /// Binaries to extract from the image during activation.
        ///
        /// Populated by the lockfile sync from each `#OCIImage`'s `extract`
        /// list. May be empty for legacy lockfiles produced before extract
        /// paths were propagated; in that case, activation will skip the
        /// image with a warning rather than extracting silently.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        extract: Vec<LockedOciExtract>,
    },
}

/// A binary extraction entry locked into the lockfile.
///
/// Mirrors the `#OCIExtract` schema fields (path/as) so the activate-path
/// in `cuenv runtime oci activate` can pull binaries out of layers without
/// re-reading the CUE module.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedOciExtract {
    /// Path to the binary inside the container (e.g., "/usr/sbin/nginx").
    pub path: String,
    /// Optional rename when exposing the binary on PATH.
    #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
    pub as_name: Option<String>,
}

impl LockedOciExtract {
    /// Compute the binary name that will be placed on PATH.
    ///
    /// Uses `as_name` if set, otherwise the final path component of
    /// `path`. Falls back to `"binary"` if neither yields a usable name.
    #[must_use]
    pub fn binary_name(&self) -> String {
        if let Some(name) = self.as_name.as_deref()
            && !name.is_empty()
        {
            return name.to_string();
        }
        self.path
            .rsplit('/')
            .find(|component| !component.is_empty())
            .unwrap_or("binary")
            .to_string()
    }
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
    /// Subdirectory of the repo that was materialized via sparse checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
    /// Tree object SHA for the materialized subdirectory at `commit:subdir`.
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
#[path = "lockfile_tests.rs"]
mod tests;
