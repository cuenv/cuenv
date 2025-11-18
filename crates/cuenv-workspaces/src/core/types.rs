//! Core types for representing workspaces, dependencies, and package managers.

use serde::{Deserialize, Serialize};
use std::fmt;
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

/// Represents a complete workspace with all its members and configuration.
///
/// A workspace is the root container for a multi-package project. It contains
/// metadata about the workspace root, the package manager in use, all member
/// packages, and the location of the lockfile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    /// The root directory of the workspace.
    pub root: PathBuf,

    /// The detected package manager.
    pub manager: PackageManager,

    /// All workspace members.
    pub members: Vec<WorkspaceMember>,

    /// Path to the lockfile, if present.
    pub lockfile: Option<PathBuf>,
}

impl Workspace {
    /// Creates a new empty workspace.
    ///
    /// # Example
    ///
    /// ```
    /// use cuenv_workspaces::{Workspace, PackageManager};
    /// use std::path::PathBuf;
    ///
    /// let workspace = Workspace::new(
    ///     PathBuf::from("/path/to/workspace"),
    ///     PackageManager::Npm,
    /// );
    /// assert_eq!(workspace.member_count(), 0);
    /// ```
    #[must_use]
    pub fn new(root: PathBuf, manager: PackageManager) -> Self {
        Self {
            root,
            manager,
            members: Vec::new(),
            lockfile: None,
        }
    }

    /// Adds a member to the workspace.
    ///
    /// # Example
    ///
    /// ```
    /// use cuenv_workspaces::{Workspace, WorkspaceMember, PackageManager};
    /// use std::path::PathBuf;
    ///
    /// let mut workspace = Workspace::new(
    ///     PathBuf::from("/workspace"),
    ///     PackageManager::Npm,
    /// );
    ///
    /// let member = WorkspaceMember {
    ///     name: "my-package".to_string(),
    ///     path: PathBuf::from("packages/my-package"),
    ///     manifest_path: PathBuf::from("packages/my-package/package.json"),
    ///     dependencies: vec![],
    /// };
    ///
    /// workspace.add_member(member);
    /// assert_eq!(workspace.member_count(), 1);
    /// ```
    pub fn add_member(&mut self, member: WorkspaceMember) {
        self.members.push(member);
    }

    /// Finds a member by name.
    ///
    /// # Example
    ///
    /// ```
    /// use cuenv_workspaces::{Workspace, WorkspaceMember, PackageManager};
    /// use std::path::PathBuf;
    ///
    /// let mut workspace = Workspace::new(
    ///     PathBuf::from("/workspace"),
    ///     PackageManager::Npm,
    /// );
    ///
    /// let member = WorkspaceMember {
    ///     name: "my-package".to_string(),
    ///     path: PathBuf::from("packages/my-package"),
    ///     manifest_path: PathBuf::from("packages/my-package/package.json"),
    ///     dependencies: vec![],
    /// };
    ///
    /// workspace.add_member(member);
    /// assert!(workspace.find_member("my-package").is_some());
    /// assert!(workspace.find_member("non-existent").is_none());
    /// ```
    #[must_use]
    pub fn find_member(&self, name: &str) -> Option<&WorkspaceMember> {
        self.members.iter().find(|m| m.name == name)
    }

    /// Returns the number of workspace members.
    ///
    /// # Example
    ///
    /// ```
    /// use cuenv_workspaces::{Workspace, PackageManager};
    /// use std::path::PathBuf;
    ///
    /// let workspace = Workspace::new(
    ///     PathBuf::from("/workspace"),
    ///     PackageManager::Npm,
    /// );
    /// assert_eq!(workspace.member_count(), 0);
    /// ```
    #[must_use]
    pub fn member_count(&self) -> usize {
        self.members.len()
    }
}

/// Represents a single package or crate within a workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMember {
    /// The name of the package/crate.
    pub name: String,

    /// Path relative to the workspace root.
    pub path: PathBuf,

    /// Path to the manifest file (package.json, Cargo.toml, etc.).
    pub manifest_path: PathBuf,

    /// Names of declared dependencies.
    pub dependencies: Vec<String>,
}

/// Identifies the package manager in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PackageManager {
    /// npm package manager
    Npm,
    /// Bun package manager
    Bun,
    /// pnpm package manager
    Pnpm,
    /// Yarn Classic (v1.x)
    YarnClassic,
    /// Yarn Modern (v2+, Berry)
    YarnModern,
    /// Cargo (Rust)
    Cargo,
}

impl PackageManager {
    /// Returns the lockfile name for this package manager.
    ///
    /// # Example
    ///
    /// ```
    /// use cuenv_workspaces::PackageManager;
    ///
    /// assert_eq!(PackageManager::Npm.lockfile_name(), "package-lock.json");
    /// assert_eq!(PackageManager::Cargo.lockfile_name(), "Cargo.lock");
    /// assert_eq!(PackageManager::Pnpm.lockfile_name(), "pnpm-lock.yaml");
    /// ```
    #[must_use]
    pub fn lockfile_name(&self) -> &str {
        match self {
            Self::Npm => "package-lock.json",
            Self::Bun => "bun.lockb",
            Self::Pnpm => "pnpm-lock.yaml",
            Self::YarnClassic | Self::YarnModern => "yarn.lock",
            Self::Cargo => "Cargo.lock",
        }
    }

    /// Returns the manifest file name for this package manager.
    ///
    /// # Example
    ///
    /// ```
    /// use cuenv_workspaces::PackageManager;
    ///
    /// assert_eq!(PackageManager::Npm.manifest_name(), "package.json");
    /// assert_eq!(PackageManager::Cargo.manifest_name(), "Cargo.toml");
    /// ```
    #[must_use]
    pub fn manifest_name(&self) -> &str {
        match self {
            Self::Npm | Self::Bun | Self::Pnpm | Self::YarnClassic | Self::YarnModern => {
                "package.json"
            }
            Self::Cargo => "Cargo.toml",
        }
    }

    /// Returns the workspace configuration file name for this package manager.
    ///
    /// # Example
    ///
    /// ```
    /// use cuenv_workspaces::PackageManager;
    ///
    /// assert_eq!(PackageManager::Npm.workspace_config_name(), "package.json");
    /// assert_eq!(PackageManager::Cargo.workspace_config_name(), "Cargo.toml");
    /// assert_eq!(PackageManager::Pnpm.workspace_config_name(), "pnpm-workspace.yaml");
    /// ```
    #[must_use]
    pub fn workspace_config_name(&self) -> &str {
        match self {
            Self::Npm | Self::Bun | Self::YarnClassic | Self::YarnModern => "package.json",
            Self::Pnpm => "pnpm-workspace.yaml",
            Self::Cargo => "Cargo.toml",
        }
    }
}

impl fmt::Display for PackageManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Npm => write!(f, "npm"),
            Self::Bun => write!(f, "bun"),
            Self::Pnpm => write!(f, "pnpm"),
            Self::YarnClassic => write!(f, "yarn-classic"),
            Self::YarnModern => write!(f, "yarn-modern"),
            Self::Cargo => write!(f, "cargo"),
        }
    }
}

/// Describes how a dependency is specified in a manifest file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workspace_new() {
        let workspace = Workspace::new(PathBuf::from("/workspace"), PackageManager::Npm);

        assert_eq!(workspace.root, PathBuf::from("/workspace"));
        assert_eq!(workspace.manager, PackageManager::Npm);
        assert_eq!(workspace.members.len(), 0);
        assert_eq!(workspace.lockfile, None);
    }

    #[test]
    fn test_workspace_add_member() {
        let mut workspace = Workspace::new(PathBuf::from("/workspace"), PackageManager::Npm);

        let member = WorkspaceMember {
            name: "pkg1".to_string(),
            path: PathBuf::from("packages/pkg1"),
            manifest_path: PathBuf::from("packages/pkg1/package.json"),
            dependencies: vec![],
        };

        workspace.add_member(member);
        assert_eq!(workspace.member_count(), 1);
    }

    #[test]
    fn test_workspace_find_member() {
        let mut workspace = Workspace::new(PathBuf::from("/workspace"), PackageManager::Npm);

        let member1 = WorkspaceMember {
            name: "pkg1".to_string(),
            path: PathBuf::from("packages/pkg1"),
            manifest_path: PathBuf::from("packages/pkg1/package.json"),
            dependencies: vec![],
        };

        let member2 = WorkspaceMember {
            name: "pkg2".to_string(),
            path: PathBuf::from("packages/pkg2"),
            manifest_path: PathBuf::from("packages/pkg2/package.json"),
            dependencies: vec!["pkg1".to_string()],
        };

        workspace.add_member(member1);
        workspace.add_member(member2);

        assert!(workspace.find_member("pkg1").is_some());
        assert!(workspace.find_member("pkg2").is_some());
        assert!(workspace.find_member("pkg3").is_none());

        let found = workspace.find_member("pkg2").unwrap();
        assert_eq!(found.name, "pkg2");
        assert_eq!(found.dependencies, vec!["pkg1"]);
    }

    #[test]
    fn test_workspace_member_count() {
        let mut workspace = Workspace::new(PathBuf::from("/workspace"), PackageManager::Cargo);
        assert_eq!(workspace.member_count(), 0);

        workspace.add_member(WorkspaceMember {
            name: "crate1".to_string(),
            path: PathBuf::from("crates/crate1"),
            manifest_path: PathBuf::from("crates/crate1/Cargo.toml"),
            dependencies: vec![],
        });
        assert_eq!(workspace.member_count(), 1);

        workspace.add_member(WorkspaceMember {
            name: "crate2".to_string(),
            path: PathBuf::from("crates/crate2"),
            manifest_path: PathBuf::from("crates/crate2/Cargo.toml"),
            dependencies: vec![],
        });
        assert_eq!(workspace.member_count(), 2);
    }

    #[test]
    fn test_workspace_serialization() {
        let mut workspace = Workspace::new(PathBuf::from("/workspace"), PackageManager::Npm);

        workspace.add_member(WorkspaceMember {
            name: "pkg1".to_string(),
            path: PathBuf::from("packages/pkg1"),
            manifest_path: PathBuf::from("packages/pkg1/package.json"),
            dependencies: vec![],
        });

        let json = serde_json::to_string(&workspace).unwrap();
        let deserialized: Workspace = serde_json::from_str(&json).unwrap();

        assert_eq!(workspace, deserialized);
    }

    #[test]
    fn test_package_manager_lockfile_name() {
        assert_eq!(PackageManager::Npm.lockfile_name(), "package-lock.json");
        assert_eq!(PackageManager::Bun.lockfile_name(), "bun.lockb");
        assert_eq!(PackageManager::Pnpm.lockfile_name(), "pnpm-lock.yaml");
        assert_eq!(PackageManager::YarnClassic.lockfile_name(), "yarn.lock");
        assert_eq!(PackageManager::YarnModern.lockfile_name(), "yarn.lock");
        assert_eq!(PackageManager::Cargo.lockfile_name(), "Cargo.lock");
    }

    #[test]
    fn test_package_manager_manifest_name() {
        assert_eq!(PackageManager::Npm.manifest_name(), "package.json");
        assert_eq!(PackageManager::Bun.manifest_name(), "package.json");
        assert_eq!(PackageManager::Pnpm.manifest_name(), "package.json");
        assert_eq!(PackageManager::YarnClassic.manifest_name(), "package.json");
        assert_eq!(PackageManager::YarnModern.manifest_name(), "package.json");
        assert_eq!(PackageManager::Cargo.manifest_name(), "Cargo.toml");
    }

    #[test]
    fn test_package_manager_workspace_config_name() {
        assert_eq!(PackageManager::Npm.workspace_config_name(), "package.json");
        assert_eq!(PackageManager::Bun.workspace_config_name(), "package.json");
        assert_eq!(
            PackageManager::Pnpm.workspace_config_name(),
            "pnpm-workspace.yaml"
        );
        assert_eq!(
            PackageManager::YarnClassic.workspace_config_name(),
            "package.json"
        );
        assert_eq!(
            PackageManager::YarnModern.workspace_config_name(),
            "package.json"
        );
        assert_eq!(PackageManager::Cargo.workspace_config_name(), "Cargo.toml");
    }

    #[test]
    fn test_package_manager_display() {
        assert_eq!(PackageManager::Npm.to_string(), "npm");
        assert_eq!(PackageManager::Bun.to_string(), "bun");
        assert_eq!(PackageManager::Pnpm.to_string(), "pnpm");
        assert_eq!(PackageManager::YarnClassic.to_string(), "yarn-classic");
        assert_eq!(PackageManager::YarnModern.to_string(), "yarn-modern");
        assert_eq!(PackageManager::Cargo.to_string(), "cargo");
    }

    #[test]
    fn test_package_manager_serialization() {
        let managers = vec![
            PackageManager::Npm,
            PackageManager::Bun,
            PackageManager::Pnpm,
            PackageManager::YarnClassic,
            PackageManager::YarnModern,
            PackageManager::Cargo,
        ];

        for manager in managers {
            let json = serde_json::to_string(&manager).unwrap();
            let deserialized: PackageManager = serde_json::from_str(&json).unwrap();
            assert_eq!(manager, deserialized);
        }
    }

    #[test]
    fn test_dependency_spec_registry() {
        let spec = DependencySpec::Registry {
            version: "^1.0.0".to_string(),
            registry: None,
        };

        let json = serde_json::to_string(&spec).unwrap();
        let deserialized: DependencySpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, deserialized);
    }

    #[test]
    fn test_dependency_spec_workspace() {
        let spec = DependencySpec::Workspace {
            path: PathBuf::from("../other-package"),
        };

        let json = serde_json::to_string(&spec).unwrap();
        let deserialized: DependencySpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, deserialized);
    }

    #[test]
    fn test_dependency_spec_git() {
        let spec = DependencySpec::Git {
            url: "https://github.com/user/repo.git".to_string(),
            rev: Some("main".to_string()),
        };

        let json = serde_json::to_string(&spec).unwrap();
        let deserialized: DependencySpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, deserialized);
    }

    #[test]
    fn test_dependency_spec_path() {
        let spec = DependencySpec::Path {
            path: PathBuf::from("../local-dep"),
        };

        let json = serde_json::to_string(&spec).unwrap();
        let deserialized: DependencySpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, deserialized);
    }

    #[test]
    fn test_lockfile_entry() {
        let entry = LockfileEntry {
            name: "example".to_string(),
            version: "1.2.3".to_string(),
            source: DependencySource::Registry("https://registry.npmjs.org/".to_string()),
            checksum: Some("sha512-abc123".to_string()),
            dependencies: vec![DependencyRef {
                name: "dep1".to_string(),
                version_req: "^2.0.0".to_string(),
            }],
            is_workspace_member: false,
        };

        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: LockfileEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, deserialized);
    }

    #[test]
    fn test_lockfile_entry_workspace_member() {
        let entry = LockfileEntry {
            name: "workspace-pkg".to_string(),
            version: "0.1.0".to_string(),
            source: DependencySource::Workspace(PathBuf::from("packages/workspace-pkg")),
            checksum: None,
            dependencies: vec![],
            is_workspace_member: true,
        };

        assert!(entry.is_workspace_member);

        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: LockfileEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, deserialized);
    }
}
