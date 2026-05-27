use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Type alias for semantic version strings.
///
/// In future versions, this may be replaced with a proper semver type.
pub type Version = String;

/// Type alias for version requirement strings.
///
/// In future versions, this may be replaced with a proper semver requirement type.
pub type VersionReq = String;

/// Type alias for URL strings.
///
/// In future versions, this may be replaced with a proper URL type.
pub type Url = String;

/// Describes how a dependency is specified in a manifest file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum DependencySpec {
    /// A registry dependency with version requirement.
    Registry {
        /// Version requirement (e.g., "^1.0.0", ">=2.0.0").
        version: VersionReq,
        /// Optional registry URL (defaults to package manager's default).
        #[serde(skip_serializing_if = "Option::is_none")]
        registry: Option<Url>,
    },

    /// A workspace-internal dependency.
    Workspace {
        /// Path to the workspace member.
        path: PathBuf,
    },

    /// A Git repository dependency.
    Git {
        /// Git repository URL.
        url: Url,
        /// Optional revision (commit, tag, or branch).
        #[serde(skip_serializing_if = "Option::is_none")]
        rev: Option<String>,
    },

    /// A local filesystem path dependency.
    Path {
        /// Path to the dependency.
        path: PathBuf,
    },
}

/// Represents a resolved dependency from a lockfile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockfileEntry {
    /// Package name.
    pub name: String,

    /// Resolved version.
    pub version: Version,

    /// Where the dependency comes from.
    pub source: DependencySource,

    /// Optional integrity checksum.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,

    /// Direct dependencies of this package.
    pub dependencies: Vec<DependencyRef>,

    /// Whether this is a workspace member.
    #[serde(default)]
    pub is_workspace_member: bool,
}

/// Describes where a dependency comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type", content = "value")]
pub enum DependencySource {
    /// Package registry URL.
    Registry(String),

    /// Git repository URL.
    Git(String),

    /// Local filesystem path.
    Path(PathBuf),

    /// Workspace member path.
    Workspace(PathBuf),
}

/// A reference to a dependency with name and version requirement.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DependencyRef {
    /// Dependency name.
    pub name: String,

    /// Version requirement as string.
    pub version_req: String,
}
