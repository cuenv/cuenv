//! Cargo manifest reading and writing.
//!
//! This module provides functionality for reading and updating version
//! information in Cargo.toml files, specifically handling the workspace
//! version inheritance pattern.

use crate::error::{Error, Result};
use crate::version::Version;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Value};

/// Handles reading and writing Cargo.toml manifest files.
pub struct CargoManifest {
    root: PathBuf,
}

impl CargoManifest {
    /// Create a new manifest handler for the given workspace root.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// Get the path to the root Cargo.toml.
    fn root_manifest_path(&self) -> PathBuf {
        self.root.join("Cargo.toml")
    }

    /// Read and parse the root Cargo.toml.
    fn read_root_manifest(&self) -> Result<DocumentMut> {
        let path = self.root_manifest_path();
        let content = fs::read_to_string(&path).map_err(|e| {
            Error::manifest(
                format!("Failed to read root Cargo.toml: {e}"),
                Some(path.clone()),
            )
        })?;
        content.parse::<DocumentMut>().map_err(|e| {
            Error::manifest(format!("Failed to parse root Cargo.toml: {e}"), Some(path))
        })
    }

    /// Read the workspace version from the root Cargo.toml.
    ///
    /// Looks for `[workspace.package].version`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or the version is not found.
    pub fn read_workspace_version(&self) -> Result<Version> {
        let doc = self.read_root_manifest()?;

        let version_str = doc
            .get("workspace")
            .and_then(|w| w.get("package"))
            .and_then(|p| p.get("version"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::manifest(
                    "No [workspace.package].version found",
                    Some(self.root_manifest_path()),
                )
            })?;

        version_str.parse::<Version>()
    }

    /// Discover all workspace member directories.
    fn discover_members(&self) -> Result<Vec<PathBuf>> {
        let doc = self.read_root_manifest()?;

        let members = doc
            .get("workspace")
            .and_then(|w| w.get("members"))
            .and_then(|m| m.as_array())
            .ok_or_else(|| {
                Error::manifest(
                    "No [workspace].members found",
                    Some(self.root_manifest_path()),
                )
            })?;

        let mut paths = Vec::new();
        for member in members {
            if let Some(pattern) = member.as_str() {
                // Handle glob patterns
                if pattern.contains('*') {
                    let full_pattern = self.root.join(pattern);
                    let pattern_str = full_pattern.to_str().ok_or_else(|| {
                        Error::manifest(
                            format!(
                                "Workspace member glob pattern contains invalid UTF-8: {}",
                                full_pattern.display()
                            ),
                            Some(full_pattern.clone()),
                        )
                    })?;
                    let matches = glob::glob(pattern_str).map_err(|e| {
                        Error::manifest(
                            format!("Invalid glob pattern: {e}"),
                            Some(full_pattern.clone()),
                        )
                    })?;
                    for entry in matches.flatten() {
                        if entry.is_dir() {
                            paths.push(entry);
                        }
                    }
                } else {
                    paths.push(self.root.join(pattern));
                }
            }
        }

        Ok(paths)
    }

    /// Get all package names with their paths in the workspace.
    ///
    /// Returns a map of package names to their root directory paths.
    ///
    /// # Errors
    ///
    /// Returns an error if workspace members cannot be discovered or parsed.
    pub fn get_package_paths(&self) -> Result<HashMap<String, PathBuf>> {
        let members = self.discover_members()?;
        let mut paths_map = HashMap::new();

        for member_path in members {
            let manifest_path = member_path.join("Cargo.toml");
            if !manifest_path.exists() {
                continue;
            }

            let content = fs::read_to_string(&manifest_path).map_err(|e| {
                Error::manifest(
                    format!("Failed to read {}: {e}", manifest_path.display()),
                    Some(manifest_path.clone()),
                )
            })?;

            let doc: toml::Value = content.parse().map_err(|e| {
                Error::manifest(
                    format!("Failed to parse {}: {e}", manifest_path.display()),
                    Some(manifest_path.clone()),
                )
            })?;

            if let Some(name) = doc
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
            {
                paths_map.insert(name.to_string(), member_path);
            }
        }

        Ok(paths_map)
    }

    /// Get all package names in the workspace.
    ///
    /// # Errors
    ///
    /// Returns an error if workspace members cannot be discovered or parsed.
    pub fn get_package_names(&self) -> Result<Vec<String>> {
        let members = self.discover_members()?;
        let mut names = Vec::new();

        for member_path in members {
            let manifest_path = member_path.join("Cargo.toml");
            if !manifest_path.exists() {
                continue;
            }

            let content = fs::read_to_string(&manifest_path).map_err(|e| {
                Error::manifest(
                    format!("Failed to read {}: {e}", manifest_path.display()),
                    Some(manifest_path.clone()),
                )
            })?;

            let doc: toml::Value = content.parse().map_err(|e| {
                Error::manifest(
                    format!("Failed to parse {}: {e}", manifest_path.display()),
                    Some(manifest_path.clone()),
                )
            })?;

            if let Some(name) = doc
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
            {
                names.push(name.to_string());
            }
        }

        Ok(names)
    }

    /// Read all package versions, resolving workspace inheritance.
    ///
    /// For packages with `version.workspace = true`, the workspace version is used.
    ///
    /// # Errors
    ///
    /// Returns an error if package manifests cannot be read or parsed.
    pub fn read_package_versions(&self) -> Result<HashMap<String, Version>> {
        let workspace_version = self.read_workspace_version()?;
        let members = self.discover_members()?;
        let mut versions = HashMap::new();

        for member_path in members {
            let manifest_path = member_path.join("Cargo.toml");
            if !manifest_path.exists() {
                continue;
            }

            let content = fs::read_to_string(&manifest_path).map_err(|e| {
                Error::manifest(
                    format!("Failed to read {}: {e}", manifest_path.display()),
                    Some(manifest_path.clone()),
                )
            })?;

            let doc: toml::Value = content.parse().map_err(|e| {
                Error::manifest(
                    format!("Failed to parse {}: {e}", manifest_path.display()),
                    Some(manifest_path.clone()),
                )
            })?;

            let Some(package) = doc.get("package") else {
                continue;
            };

            let Some(name) = package.get("name").and_then(|n| n.as_str()) else {
                continue;
            };

            // Check if version uses workspace inheritance
            let version =
                if let Some(version_table) = package.get("version").and_then(|v| v.as_table()) {
                    if version_table.get("workspace").and_then(|w| w.as_bool()) == Some(true) {
                        workspace_version.clone()
                    } else {
                        // Shouldn't happen, but handle it
                        continue;
                    }
                } else if let Some(version_str) = package.get("version").and_then(|v| v.as_str()) {
                    version_str.parse::<Version>()?
                } else {
                    continue;
                };

            versions.insert(name.to_string(), version);
        }

        Ok(versions)
    }

    /// Update the workspace version in the root Cargo.toml.
    ///
    /// This updates `[workspace.package].version`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read, parsed, or written.
    pub fn update_workspace_version(&self, new_version: &Version) -> Result<()> {
        let path = self.root_manifest_path();
        let content = fs::read_to_string(&path).map_err(|e| {
            Error::manifest(
                format!("Failed to read root Cargo.toml: {e}"),
                Some(path.clone()),
            )
        })?;

        let mut doc = content.parse::<DocumentMut>().map_err(|e| {
            Error::manifest(
                format!("Failed to parse root Cargo.toml: {e}"),
                Some(path.clone()),
            )
        })?;

        // Update [workspace.package].version
        if let Some(workspace) = doc.get_mut("workspace") {
            if let Some(package) = workspace.get_mut("package") {
                if let Item::Table(pkg_table) = package {
                    pkg_table["version"] = toml_edit::value(new_version.to_string());
                }
            }
        }

        fs::write(&path, doc.to_string()).map_err(|e| {
            Error::manifest(format!("Failed to write root Cargo.toml: {e}"), Some(path))
        })?;

        Ok(())
    }

    /// Read package dependencies from member manifests.
    ///
    /// Returns a map of package names to their workspace-internal dependencies.
    ///
    /// # Errors
    ///
    /// Returns an error if package manifests cannot be read or parsed.
    pub fn read_package_dependencies(&self) -> Result<HashMap<String, Vec<String>>> {
        let members = self.discover_members()?;
        let mut dependencies_map = HashMap::new();

        for member_path in members {
            let manifest_path = member_path.join("Cargo.toml");
            if !manifest_path.exists() {
                continue;
            }

            let content = fs::read_to_string(&manifest_path).map_err(|e| {
                Error::manifest(
                    format!("Failed to read {}: {e}", manifest_path.display()),
                    Some(manifest_path.clone()),
                )
            })?;

            let doc: toml::Value = content.parse().map_err(|e| {
                Error::manifest(
                    format!("Failed to parse {}: {e}", manifest_path.display()),
                    Some(manifest_path.clone()),
                )
            })?;

            let Some(package) = doc.get("package") else {
                continue;
            };

            let Some(name) = package.get("name").and_then(|n| n.as_str()) else {
                continue;
            };

            // Collect workspace-internal dependencies
            let mut deps = Vec::new();

            // Check [dependencies]
            if let Some(dependencies) = doc.get("dependencies").and_then(|d| d.as_table()) {
                for (dep_name, dep_value) in dependencies {
                    // Check if it's a path dependency (workspace-internal)
                    if let Some(dep_table) = dep_value.as_table() {
                        if dep_table.contains_key("path")
                            || dep_table.get("workspace") == Some(&toml::Value::Boolean(true))
                        {
                            deps.push(dep_name.clone());
                        }
                    }
                }
            }

            dependencies_map.insert(name.to_string(), deps);
        }

        Ok(dependencies_map)
    }

    /// Update workspace dependency versions in the root Cargo.toml.
    ///
    /// This updates versions in `[workspace.dependencies]` for internal crates.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read, parsed, or written.
    pub fn update_workspace_dependency_versions(
        &self,
        packages: &HashMap<String, Version>,
    ) -> Result<()> {
        let path = self.root_manifest_path();
        let content = fs::read_to_string(&path).map_err(|e| {
            Error::manifest(
                format!("Failed to read root Cargo.toml: {e}"),
                Some(path.clone()),
            )
        })?;

        let mut doc = content.parse::<DocumentMut>().map_err(|e| {
            Error::manifest(
                format!("Failed to parse root Cargo.toml: {e}"),
                Some(path.clone()),
            )
        })?;

        // Update [workspace.dependencies]
        if let Some(workspace) = doc.get_mut("workspace") {
            if let Some(deps) = workspace.get_mut("dependencies") {
                if let Item::Table(deps_table) = deps {
                    for (pkg_name, version) in packages {
                        if let Some(dep) = deps_table.get_mut(pkg_name) {
                            // Dependencies can be either inline tables or dotted keys
                            match dep {
                                Item::Value(Value::InlineTable(table)) => {
                                    table.insert("version", new_version_value(version));
                                }
                                Item::Table(table) => {
                                    table["version"] = toml_edit::value(version.to_string());
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        fs::write(&path, doc.to_string()).map_err(|e| {
            Error::manifest(format!("Failed to write root Cargo.toml: {e}"), Some(path))
        })?;

        Ok(())
    }
}

/// Create a new TOML value for a version string.
fn new_version_value(version: &Version) -> Value {
    Value::from(version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_workspace(temp: &TempDir) -> PathBuf {
        let root = temp.path().to_path_buf();

        // Create root Cargo.toml
        let root_manifest = r#"
[workspace]
resolver = "2"
members = ["crates/foo", "crates/bar"]

[workspace.package]
version = "1.2.3"
edition = "2021"

[workspace.dependencies]
foo = { path = "crates/foo", version = "1.2.3" }
bar = { path = "crates/bar", version = "1.2.3" }
"#;
        fs::write(root.join("Cargo.toml"), root_manifest).unwrap();

        // Create member crates
        fs::create_dir_all(root.join("crates/foo")).unwrap();
        fs::create_dir_all(root.join("crates/bar")).unwrap();

        let foo_manifest = r#"
[package]
name = "foo"
version.workspace = true
"#;
        fs::write(root.join("crates/foo/Cargo.toml"), foo_manifest).unwrap();

        let bar_manifest = r#"
[package]
name = "bar"
version.workspace = true
"#;
        fs::write(root.join("crates/bar/Cargo.toml"), bar_manifest).unwrap();

        root
    }

    #[test]
    fn test_read_workspace_version() {
        let temp = TempDir::new().unwrap();
        let root = create_test_workspace(&temp);

        let manifest = CargoManifest::new(&root);
        let version = manifest.read_workspace_version().unwrap();

        assert_eq!(version, Version::new(1, 2, 3));
    }

    #[test]
    fn test_get_package_names() {
        let temp = TempDir::new().unwrap();
        let root = create_test_workspace(&temp);

        let manifest = CargoManifest::new(&root);
        let mut names = manifest.get_package_names().unwrap();
        names.sort();

        assert_eq!(names, vec!["bar", "foo"]);
    }

    #[test]
    fn test_read_package_versions() {
        let temp = TempDir::new().unwrap();
        let root = create_test_workspace(&temp);

        let manifest = CargoManifest::new(&root);
        let versions = manifest.read_package_versions().unwrap();

        assert_eq!(versions.get("foo"), Some(&Version::new(1, 2, 3)));
        assert_eq!(versions.get("bar"), Some(&Version::new(1, 2, 3)));
    }

    #[test]
    fn test_update_workspace_version() {
        let temp = TempDir::new().unwrap();
        let root = create_test_workspace(&temp);

        let manifest = CargoManifest::new(&root);
        manifest
            .update_workspace_version(&Version::new(2, 0, 0))
            .unwrap();

        // Read back and verify
        let new_version = manifest.read_workspace_version().unwrap();
        assert_eq!(new_version, Version::new(2, 0, 0));
    }

    #[test]
    fn test_update_workspace_dependency_versions() {
        let temp = TempDir::new().unwrap();
        let root = create_test_workspace(&temp);

        let manifest = CargoManifest::new(&root);
        let packages = HashMap::from([
            ("foo".to_string(), Version::new(2, 0, 0)),
            ("bar".to_string(), Version::new(2, 0, 0)),
        ]);
        manifest
            .update_workspace_dependency_versions(&packages)
            .unwrap();

        // Read back and verify the content was updated
        let content = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(content.contains("version = \"2.0.0\""));
    }
}
