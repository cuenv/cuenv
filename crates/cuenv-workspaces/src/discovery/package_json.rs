//! Discovery implementation for npm/Bun/Yarn workspaces via package.json.

use crate::core::traits::WorkspaceDiscovery;
use crate::core::types::{PackageManager, Workspace, WorkspaceMember};
use crate::discovery::{read_json_file, resolve_glob_patterns};
use crate::error::{Error, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Discovers workspaces configured in `package.json`.
///
/// This handles the `workspaces` field in `package.json`, which is supported by
/// npm, Bun, and Yarn. It supports both the array format and the object format
/// (with `packages` key).
pub struct PackageJsonDiscovery;

impl WorkspaceDiscovery for PackageJsonDiscovery {
    fn discover(&self, root: &Path) -> Result<Workspace> {
        let package_json_path = root.join("package.json");
        if !package_json_path.exists() {
            return Err(Error::WorkspaceNotFound {
                path: root.to_path_buf(),
            });
        }

        let package_json: PackageJson = read_json_file(&package_json_path)?;
        
        // If no workspaces field, verify it's a valid package but return empty workspace
        // logic could vary, but usually a workspace needs the workspaces field
        if package_json.workspaces.is_none() {
            // It's a valid single package, but treat as empty workspace for now
            // or potentially a workspace with just the root member?
            // For now, we'll return a workspace with no members if workspaces field is missing,
            // unless we want to treat single-repo as a workspace of 1.
            // The prompt says: "If no workspaces field, return empty workspace (single-package repo)"
            // We also need to detect the manager.
            let manager = detect_manager(root);
            let mut workspace = Workspace::new(root.to_path_buf(), manager);
            workspace.lockfile = find_lockfile(root, manager);
            return Ok(workspace);
        }

        let members = self.find_members(root)?;
        let manager = detect_manager(root);
        
        let mut workspace = Workspace::new(root.to_path_buf(), manager);
        workspace.members = members;
        workspace.lockfile = find_lockfile(root, manager);

        Ok(workspace)
    }

    fn find_members(&self, root: &Path) -> Result<Vec<WorkspaceMember>> {
        let package_json_path = root.join("package.json");
        let package_json: PackageJson = read_json_file(&package_json_path)?;

        let patterns = match package_json.workspaces {
            Some(WorkspacesField::Array(patterns)) => patterns,
            Some(WorkspacesField::Object { packages, .. }) => packages,
            None => return Ok(Vec::new()),
        };

        let matched_paths = resolve_glob_patterns(root, &patterns, &[])?;
        let mut members = Vec::new();

        for path in matched_paths {
            if self.validate_member(&path)? {
                let manifest_path = path.join("package.json");
                let member_pkg: PackageJson = read_json_file(&manifest_path)?;
                
                if let Some(name) = member_pkg.name {
                    let mut dependencies = Vec::new();
                    if let Some(deps) = member_pkg.dependencies {
                        dependencies.extend(deps.keys().cloned());
                    }
                    if let Some(dev_deps) = member_pkg.dev_dependencies {
                        dependencies.extend(dev_deps.keys().cloned());
                    }

                    members.push(WorkspaceMember {
                        name,
                        path: path.strip_prefix(root).unwrap_or(&path).to_path_buf(),
                        manifest_path,
                        dependencies,
                    });
                }
            }
        }

        members.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(members)
    }

    /// Validates whether a directory is a valid workspace member.
    ///
    /// # Tolerant Validation Behavior
    ///
    /// This method silently skips (returns `Ok(false)`) for:
    /// - Directories without a `package.json` file
    /// - `package.json` files with invalid JSON syntax
    /// - `package.json` files missing a `name` field
    ///
    /// Only I/O errors (permission issues, etc.) are propagated as `Err`.
    /// This tolerant approach allows workspace discovery to succeed even when
    /// some member directories are malformed, including only valid members in the result.
    fn validate_member(&self, member_path: &Path) -> Result<bool> {
        let manifest_path = member_path.join("package.json");
        if !manifest_path.exists() {
            return Ok(false);
        }

        // Try parsing to ensure it has a name
        match read_json_file::<PackageJson>(&manifest_path) {
            Ok(pkg) => {
                if pkg.name.is_some() {
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            Err(Error::Json { .. }) => Ok(false), // Invalid JSON: silently skip this member
            Err(e) => Err(e), // I/O error: propagate
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageJson {
    name: Option<String>,
    workspaces: Option<WorkspacesField>,
    dependencies: Option<HashMap<String, String>>,
    dev_dependencies: Option<HashMap<String, String>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WorkspacesField {
    Array(Vec<String>),
    Object { packages: Vec<String> },
}

fn detect_manager(root: &Path) -> PackageManager {
    if root.join("bun.lockb").exists() {
        PackageManager::Bun
    } else if root.join("yarn.lock").exists() {
        // Could distinguish classic/modern by parsing, but for now default to modern or classic?
        // The PackageManager enum has both. Let's pick one or check version.
        // For simplicity, if we can't tell, maybe default to YarnClassic as it's more common in older repos,
        // or check .yarnrc.yml for modern.
        if root.join(".yarnrc.yml").exists() {
            PackageManager::YarnModern
        } else {
            PackageManager::YarnClassic
        }
    } else if root.join("pnpm-lock.yaml").exists() {
        PackageManager::Pnpm
    } else {
        PackageManager::Npm
    }
}

fn find_lockfile(root: &Path, manager: PackageManager) -> Option<std::path::PathBuf> {
    let lockfile = root.join(manager.lockfile_name());
    if lockfile.exists() {
        Some(lockfile)
    } else {
        None
    }
}
