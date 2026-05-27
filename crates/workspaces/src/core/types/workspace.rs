use super::package_manager::PackageManager;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Represents a complete workspace with all its members and configuration.
///
/// A workspace is the root container for a multi-package project. It contains
/// metadata about the workspace root, the package manager in use, all member
/// packages, and the location of the lockfile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    pub const fn new(root: PathBuf, manager: PackageManager) -> Self {
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
    pub const fn member_count(&self) -> usize {
        self.members.len()
    }

    /// Checks if a path is a member of this workspace.
    ///
    /// Returns true if the path matches or is contained within any workspace member path.
    ///
    /// # Example
    ///
    /// ```
    /// use cuenv_workspaces::{Workspace, WorkspaceMember, PackageManager};
    /// use std::path::PathBuf;
    ///
    /// let mut workspace = Workspace::new(
    ///     PathBuf::from("/workspace"),
    ///     PackageManager::Bun,
    /// );
    ///
    /// let member = WorkspaceMember {
    ///     name: "website".to_string(),
    ///     path: PathBuf::from("projects/website"),
    ///     manifest_path: PathBuf::from("projects/website/package.json"),
    ///     dependencies: vec![],
    /// };
    ///
    /// workspace.add_member(member);
    /// assert!(workspace.contains_path(&PathBuf::from("projects/website")));
    /// assert!(!workspace.contains_path(&PathBuf::from("other/project")));
    /// ```
    #[must_use]
    pub fn contains_path(&self, path: &std::path::Path) -> bool {
        self.members
            .iter()
            .any(|m| m.path == path || path.starts_with(&m.path) || m.path.starts_with(path))
    }

    /// Finds a workspace member by its path.
    ///
    /// # Example
    ///
    /// ```
    /// use cuenv_workspaces::{Workspace, WorkspaceMember, PackageManager};
    /// use std::path::PathBuf;
    ///
    /// let mut workspace = Workspace::new(
    ///     PathBuf::from("/workspace"),
    ///     PackageManager::Bun,
    /// );
    ///
    /// let member = WorkspaceMember {
    ///     name: "website".to_string(),
    ///     path: PathBuf::from("projects/website"),
    ///     manifest_path: PathBuf::from("projects/website/package.json"),
    ///     dependencies: vec![],
    /// };
    ///
    /// workspace.add_member(member);
    /// assert!(workspace.find_member_by_path(&PathBuf::from("projects/website")).is_some());
    /// assert!(workspace.find_member_by_path(&PathBuf::from("other/path")).is_none());
    /// ```
    #[must_use]
    pub fn find_member_by_path(&self, path: &Path) -> Option<&WorkspaceMember> {
        self.members.iter().find(|m| m.path == path)
    }

    /// Returns the paths of all workspace members that the given member depends on,
    /// including transitive dependencies.
    ///
    /// # Example
    ///
    /// ```
    /// use cuenv_workspaces::{Workspace, WorkspaceMember, PackageManager};
    /// use std::path::PathBuf;
    ///
    /// let mut workspace = Workspace::new(
    ///     PathBuf::from("/workspace"),
    ///     PackageManager::Bun,
    /// );
    ///
    /// workspace.add_member(WorkspaceMember {
    ///     name: "shared".to_string(),
    ///     path: PathBuf::from("packages/shared"),
    ///     manifest_path: PathBuf::from("packages/shared/package.json"),
    ///     dependencies: vec![],
    /// });
    ///
    /// workspace.add_member(WorkspaceMember {
    ///     name: "app".to_string(),
    ///     path: PathBuf::from("packages/app"),
    ///     manifest_path: PathBuf::from("packages/app/package.json"),
    ///     dependencies: vec!["shared".to_string()],
    /// });
    ///
    /// let deps = workspace.resolve_workspace_dependency_paths("app");
    /// assert_eq!(deps.len(), 1);
    /// assert!(deps.contains(&PathBuf::from("packages/shared")));
    /// ```
    #[must_use]
    pub fn resolve_workspace_dependency_paths(&self, member_name: &str) -> HashSet<PathBuf> {
        let mut paths = HashSet::new();
        let mut visited = HashSet::new();
        self.collect_workspace_deps_recursive(member_name, &mut paths, &mut visited);
        paths
    }

    /// Recursively collects workspace dependency paths, preventing infinite loops
    /// from circular dependencies.
    fn collect_workspace_deps_recursive(
        &self,
        member_name: &str,
        paths: &mut HashSet<PathBuf>,
        visited: &mut HashSet<String>,
    ) {
        if !visited.insert(member_name.to_string()) {
            return; // Already visited - prevent cycles
        }

        if let Some(member) = self.find_member(member_name) {
            for dep_name in &member.dependencies {
                if visited.contains(dep_name) {
                    continue;
                }
                if let Some(dep_member) = self.find_member(dep_name) {
                    paths.insert(dep_member.path.clone());
                    self.collect_workspace_deps_recursive(dep_name, paths, visited);
                }
            }
        }
    }
}

/// Represents a single package or crate within a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
