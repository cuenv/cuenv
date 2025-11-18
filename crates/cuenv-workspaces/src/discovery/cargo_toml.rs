//! Discovery implementation for Cargo workspaces via Cargo.toml.
//!
//! # Behavior for Missing `[workspace]` Section
//!
//! When discovering a Cargo project, this implementation treats a `Cargo.toml` without
//! a `[workspace]` section as a valid empty workspace (single-package repository).
//! This is consistent with Cargo's behavior where:
//! - A `Cargo.toml` with `[workspace]` defines a workspace
//! - A `Cargo.toml` without `[workspace]` is a single-package project
//!
//! In both cases, discovery succeeds but returns zero members for the single-package case,
//! as single-package projects don't have workspace members in the traditional sense.

use crate::core::traits::WorkspaceDiscovery;
use crate::core::types::{PackageManager, Workspace, WorkspaceMember};
use crate::discovery::{read_toml_file, resolve_glob_patterns};
use crate::error::{Error, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use toml::Value;

/// Discovers workspaces configured in `Cargo.toml`.
///
/// # Discovery Behavior
///
/// - **With `[workspace]` section**: Parses workspace members according to the `members` field,
///   respecting any `exclude` patterns.
/// - **Without `[workspace]` section**: Treats the project as a valid empty workspace
///   (single-package repository) and returns successfully with zero members.
///
/// This behavior aligns with Cargo's semantics where a `[workspace]` section explicitly
/// defines a multi-package workspace.
pub struct CargoTomlDiscovery;

impl WorkspaceDiscovery for CargoTomlDiscovery {
    fn discover(&self, root: &Path) -> Result<Workspace> {
        let cargo_toml_path = root.join("Cargo.toml");
        if !cargo_toml_path.exists() {
            return Err(Error::WorkspaceNotFound {
                path: root.to_path_buf(),
            });
        }

        let cargo_toml: CargoToml = read_toml_file(&cargo_toml_path)?;

        if cargo_toml.workspace.is_none() {
            // No [workspace] section: treat as a valid empty workspace (single-package repository).
            // This is consistent with Cargo's behavior where the [workspace] section explicitly
            // defines a multi-package workspace. Without it, we have a single-package project,
            // which we represent as an empty workspace (zero members).
            let mut workspace = Workspace::new(root.to_path_buf(), PackageManager::Cargo);
            workspace.lockfile = root.join("Cargo.lock").exists().then(|| root.join("Cargo.lock"));
            return Ok(workspace);
        }

        let members = self.find_members(root)?;
        
        let mut workspace = Workspace::new(root.to_path_buf(), PackageManager::Cargo);
        workspace.members = members;
        
        let lockfile = root.join("Cargo.lock");
        if lockfile.exists() {
            workspace.lockfile = Some(lockfile);
        }

        Ok(workspace)
    }

    fn find_members(&self, root: &Path) -> Result<Vec<WorkspaceMember>> {
        let cargo_toml_path = root.join("Cargo.toml");
        let cargo_toml: CargoToml = read_toml_file(&cargo_toml_path)?;

        // If [workspace] is missing, return an empty list (single-package repository).
        // This is consistent with discover() treating missing [workspace] as a valid
        // empty workspace rather than an error.
        let workspace = match cargo_toml.workspace {
            Some(ws) => ws,
            None => return Ok(Vec::new()),
        };

        let exclusions = workspace.exclude.unwrap_or_default();
        let matched_paths = resolve_glob_patterns(root, &workspace.members, &exclusions)?;
        let mut members = Vec::new();

        for path in matched_paths {
            if self.validate_member(&path)? {
                let manifest_path = path.join("Cargo.toml");
                let member_pkg: CargoToml = read_toml_file(&manifest_path)?;
                
                if let Some(package) = member_pkg.package {
                    let mut dependencies = Vec::new();
                    if let Some(deps) = member_pkg.dependencies {
                        dependencies.extend(deps.keys().cloned());
                    }
                    if let Some(dev_deps) = member_pkg.dev_dependencies {
                        dependencies.extend(dev_deps.keys().cloned());
                    }

                    members.push(WorkspaceMember {
                        name: package.name,
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
    /// - Directories without a `Cargo.toml` file
    /// - `Cargo.toml` files with invalid TOML syntax
    /// - `Cargo.toml` files missing a `[package]` section or package name
    ///
    /// Only I/O errors (permission issues, etc.) are propagated as `Err`.
    /// This tolerant approach allows workspace discovery to succeed even when
    /// some member directories are malformed, including only valid members in the result.
    fn validate_member(&self, member_path: &Path) -> Result<bool> {
        let manifest_path = member_path.join("Cargo.toml");
        if !manifest_path.exists() {
            return Ok(false);
        }

        // Try parsing to ensure it has a [package] section with a name
        match read_toml_file::<CargoToml>(&manifest_path) {
            Ok(pkg) => {
                if pkg.package.map(|p| !p.name.is_empty()).unwrap_or(false) {
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            Err(Error::Toml { .. }) => Ok(false), // Invalid TOML: silently skip this member
            Err(e) => Err(e), // I/O error: propagate
        }
    }
}

#[derive(Deserialize)]
struct CargoToml {
    workspace: Option<WorkspaceSection>,
    package: Option<PackageSection>,
    dependencies: Option<HashMap<String, Value>>,
    #[serde(rename = "dev-dependencies")]
    dev_dependencies: Option<HashMap<String, Value>>,
}

#[derive(Deserialize)]
struct WorkspaceSection {
    members: Vec<String>,
    exclude: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct PackageSection {
    name: String,
}
