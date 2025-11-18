//! Discovery implementation for pnpm workspaces via pnpm-workspace.yaml.

use crate::core::traits::WorkspaceDiscovery;
use crate::core::types::{PackageManager, Workspace, WorkspaceMember};
use crate::discovery::{read_json_file, read_yaml_file, resolve_glob_patterns};
use crate::error::{Error, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Discovers workspaces configured in `pnpm-workspace.yaml`.
pub struct PnpmWorkspaceDiscovery;

impl WorkspaceDiscovery for PnpmWorkspaceDiscovery {
    fn discover(&self, root: &Path) -> Result<Workspace> {
        let workspace_yaml_path = root.join("pnpm-workspace.yaml");
        if !workspace_yaml_path.exists() {
            return Err(Error::WorkspaceNotFound {
                path: root.to_path_buf(),
            });
        }

        // We don't really need the content here except to validate it parses
        let _: PnpmWorkspace = read_yaml_file(&workspace_yaml_path)?;

        let members = self.find_members(root)?;
        
        let mut workspace = Workspace::new(root.to_path_buf(), PackageManager::Pnpm);
        workspace.members = members;
        
        let lockfile = root.join("pnpm-lock.yaml");
        if lockfile.exists() {
            workspace.lockfile = Some(lockfile);
        }

        Ok(workspace)
    }

    fn find_members(&self, root: &Path) -> Result<Vec<WorkspaceMember>> {
        let workspace_yaml_path = root.join("pnpm-workspace.yaml");
        let workspace_config: PnpmWorkspace = read_yaml_file(&workspace_yaml_path)?;

        let matched_paths = resolve_glob_patterns(root, &workspace_config.packages, &[])?;
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
                    if let Some(peer_deps) = member_pkg.peer_dependencies {
                        dependencies.extend(peer_deps.keys().cloned());
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
struct PnpmWorkspace {
    packages: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageJson {
    name: Option<String>,
    dependencies: Option<HashMap<String, String>>,
    dev_dependencies: Option<HashMap<String, String>>,
    peer_dependencies: Option<HashMap<String, String>>,
}
