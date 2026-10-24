use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

/// Defines a string newtype with the conversions and comparisons that every
/// dependency string type shares.
macro_rules! string_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates a value from any string.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Returns the value as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Returns the underlying string.
            #[must_use]
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl PartialEq<String> for $name {
            fn eq(&self, other: &String) -> bool {
                &self.0 == other
            }
        }
    };
}

string_newtype! {
    /// A resolved package version as a lockfile records it.
    ///
    /// Not necessarily semver: lockfiles also record git revisions,
    /// `workspace:*` protocols and other package-manager-specific forms.
    Version
}

string_newtype! {
    /// A version requirement as a manifest declares it (e.g. `^1.0.0`,
    /// `>=2.0.0`, `workspace:*`).
    VersionReq
}

string_newtype! {
    /// A registry or repository URL as a manifest or lockfile records it.
    ///
    /// Kept as text rather than a parsed URL because package managers record
    /// forms such as `git+ssh://` and `registry+https://` that are not always
    /// valid URLs.
    Url
}

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
#[non_exhaustive]
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
