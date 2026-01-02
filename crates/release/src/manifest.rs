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

            let doc: toml::Value = toml::from_str(&content).map_err(|e| {
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

            let doc: toml::Value = toml::from_str(&content).map_err(|e| {
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

            let doc: toml::Value = toml::from_str(&content).map_err(|e| {
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
            let version = if let Some(version_table) =
                package.get("version").and_then(|v| v.as_table())
                && version_table
                    .get("workspace")
                    .is_some_and(|w| w.as_bool() == Some(true))
            {
                workspace_version.clone()
            } else if package.get("version").and_then(|v| v.as_table()).is_some() {
                // Edge case: 'version' is a table, but not a workspace inheritance table (i.e., not { workspace = true }).
                // This is not a valid Cargo manifest configuration; skip this package.
                continue;
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
        if let Some(workspace) = doc.get_mut("workspace")
            && let Some(package) = workspace.get_mut("package")
            && let Item::Table(pkg_table) = package
        {
            pkg_table["version"] = toml_edit::value(new_version.to_string());
        } else {
            return Err(Error::manifest(
                "Root manifest is missing [workspace.package] table".to_string(),
                Some(path),
            ));
        }

        fs::write(&path, doc.to_string()).map_err(|e| {
            Error::manifest(format!("Failed to write root Cargo.toml: {e}"), Some(path))
        })?;

        Ok(())
    }

    /// Read package dependencies from member manifests.
    ///
    /// Returns a map of package names to their workspace-internal dependencies.
    /// Only includes dependencies that are defined in `[workspace.dependencies]`
    /// with a `path` key, indicating they are internal to the workspace.
    ///
    /// # Errors
    ///
    /// Returns an error if package manifests cannot be read or parsed.
    pub fn read_package_dependencies(&self) -> Result<HashMap<String, Vec<String>>> {
        // Step 1: Read workspace.dependencies to find internal packages
        // Internal packages are those defined with `path = ...` in workspace.dependencies
        let internal_packages = self.get_internal_package_names()?;

        // Step 2: For each member, collect only internal workspace dependencies
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

            let doc: toml::Value = toml::from_str(&content).map_err(|e| {
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
                    // Check if it's a workspace dependency that references an internal package
                    if let Some(dep_table) = dep_value.as_table() {
                        let is_workspace =
                            dep_table.get("workspace") == Some(&toml::Value::Boolean(true));
                        let has_path = dep_table.contains_key("path");
                        let is_internal = internal_packages.contains(dep_name);

                        // Include if: has explicit path OR (is workspace dep AND is internal package)
                        if has_path || (is_workspace && is_internal) {
                            deps.push(dep_name.clone());
                        }
                    }
                }
            }

            dependencies_map.insert(name.to_string(), deps);
        }

        Ok(dependencies_map)
    }

    /// Get the names of internal packages from `[workspace.dependencies]`.
    ///
    /// Internal packages are those defined with a `path` key.
    fn get_internal_package_names(&self) -> Result<std::collections::HashSet<String>> {
        use std::collections::HashSet;

        let doc = self.read_root_manifest()?;
        let mut internal_packages = HashSet::new();

        if let Some(workspace) = doc.get("workspace")
            && let Some(deps) = workspace.get("dependencies")
            && let Some(deps_table) = deps.as_table()
        {
            for (name, value) in deps_table {
                // Check for inline table: { path = "...", version = "..." }
                if let Some(inline) = value.as_inline_table()
                    && inline.contains_key("path")
                {
                    internal_packages.insert(name.to_string());
                }
                // Check for regular table (dotted keys)
                if let Some(table) = value.as_table()
                    && table.contains_key("path")
                {
                    internal_packages.insert(name.to_string());
                }
            }
        }

        Ok(internal_packages)
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
        if let Some(workspace) = doc.get_mut("workspace")
            && let Some(deps) = workspace.get_mut("dependencies")
            && let Item::Table(deps_table) = deps
        {
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
        // Note: If [workspace.dependencies] doesn't exist, that's OK - not all workspaces use it

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
        let root_manifest = r#"[workspace]
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

        let foo_manifest = r#"[package]
name = "foo"
version.workspace = true
"#;
        fs::write(root.join("crates/foo/Cargo.toml"), foo_manifest).unwrap();

        let bar_manifest = r#"[package]
name = "bar"
version.workspace = true
"#;
        fs::write(root.join("crates/bar/Cargo.toml"), bar_manifest).unwrap();

        root
    }

    // ==========================================================================
    // CargoManifest construction tests
    // ==========================================================================

    #[test]
    fn test_cargo_manifest_new() {
        let temp = TempDir::new().unwrap();
        let manifest = CargoManifest::new(temp.path());
        // Just verify construction works
        let _ = manifest;
    }

    // ==========================================================================
    // read_workspace_version tests
    // ==========================================================================

    #[test]
    fn test_read_workspace_version() {
        let temp = TempDir::new().unwrap();
        let root = create_test_workspace(&temp);

        let manifest = CargoManifest::new(&root);
        let version = manifest.read_workspace_version().unwrap();

        assert_eq!(version, Version::new(1, 2, 3));
    }

    #[test]
    fn test_read_workspace_version_no_file() {
        let temp = TempDir::new().unwrap();
        let manifest = CargoManifest::new(temp.path());

        let result = manifest.read_workspace_version();
        assert!(result.is_err());
    }

    #[test]
    fn test_read_workspace_version_no_workspace_package() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();

        // Create a minimal Cargo.toml without workspace.package.version
        let manifest_content = r#"[workspace]
members = ["crates/foo"]
"#;
        fs::write(root.join("Cargo.toml"), manifest_content).unwrap();

        let manifest = CargoManifest::new(&root);
        let result = manifest.read_workspace_version();
        assert!(result.is_err());
    }

    // ==========================================================================
    // get_package_names tests
    // ==========================================================================

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
    fn test_get_package_names_no_members() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();

        // No [workspace].members
        let manifest_content = r#"[workspace]
resolver = "2"

[workspace.package]
version = "1.0.0"
"#;
        fs::write(root.join("Cargo.toml"), manifest_content).unwrap();

        let manifest = CargoManifest::new(&root);
        let result = manifest.get_package_names();
        assert!(result.is_err());
    }

    #[test]
    fn test_get_package_names_missing_member_cargo_toml() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();

        // Create workspace that references non-existent member
        let manifest_content = r#"[workspace]
resolver = "2"
members = ["crates/nonexistent"]

[workspace.package]
version = "1.0.0"
"#;
        fs::write(root.join("Cargo.toml"), manifest_content).unwrap();
        fs::create_dir_all(root.join("crates/nonexistent")).unwrap();
        // Don't create the member's Cargo.toml

        let manifest = CargoManifest::new(&root);
        let names = manifest.get_package_names().unwrap();
        // Should return empty since the member has no Cargo.toml
        assert!(names.is_empty());
    }

    // ==========================================================================
    // get_package_paths tests
    // ==========================================================================

    #[test]
    fn test_get_package_paths() {
        let temp = TempDir::new().unwrap();
        let root = create_test_workspace(&temp);

        let manifest = CargoManifest::new(&root);
        let paths = manifest.get_package_paths().unwrap();

        assert!(paths.contains_key("foo"));
        assert!(paths.contains_key("bar"));
        assert!(paths.get("foo").unwrap().ends_with("crates/foo"));
    }

    // ==========================================================================
    // read_package_versions tests
    // ==========================================================================

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
    fn test_read_package_versions_explicit_version() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();

        // Create workspace
        let root_manifest = r#"[workspace]
resolver = "2"
members = ["crates/explicit"]

[workspace.package]
version = "1.0.0"
"#;
        fs::write(root.join("Cargo.toml"), root_manifest).unwrap();

        // Create member with explicit version
        fs::create_dir_all(root.join("crates/explicit")).unwrap();
        let member_manifest = r#"[package]
name = "explicit"
version = "2.0.0"
"#;
        fs::write(root.join("crates/explicit/Cargo.toml"), member_manifest).unwrap();

        let manifest = CargoManifest::new(&root);
        let versions = manifest.read_package_versions().unwrap();

        assert_eq!(versions.get("explicit"), Some(&Version::new(2, 0, 0)));
    }

    // ==========================================================================
    // update_workspace_version tests
    // ==========================================================================

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
    fn test_update_workspace_version_no_workspace_package() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();

        // Create a minimal Cargo.toml without [workspace.package]
        let manifest_content = r#"[workspace]
members = ["crates/foo"]
"#;
        fs::write(root.join("Cargo.toml"), manifest_content).unwrap();

        let manifest = CargoManifest::new(&root);
        let result = manifest.update_workspace_version(&Version::new(2, 0, 0));
        assert!(result.is_err());
    }

    // ==========================================================================
    // update_workspace_dependency_versions tests
    // ==========================================================================

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

    #[test]
    fn test_update_workspace_dependency_versions_no_deps() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();

        // Create workspace without [workspace.dependencies]
        let manifest_content = r#"[workspace]
resolver = "2"
members = []

[workspace.package]
version = "1.0.0"
"#;
        fs::write(root.join("Cargo.toml"), manifest_content).unwrap();

        let manifest = CargoManifest::new(&root);
        let packages = HashMap::from([("foo".to_string(), Version::new(2, 0, 0))]);

        // Should succeed silently - no workspace.dependencies to update
        manifest
            .update_workspace_dependency_versions(&packages)
            .unwrap();
    }

    #[test]
    fn test_update_workspace_dependency_versions_partial_update() {
        let temp = TempDir::new().unwrap();
        let root = create_test_workspace(&temp);

        let manifest = CargoManifest::new(&root);
        // Only update foo, not bar
        let packages = HashMap::from([("foo".to_string(), Version::new(3, 0, 0))]);
        manifest
            .update_workspace_dependency_versions(&packages)
            .unwrap();

        // Read back and verify foo was updated
        let content = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(content.contains("version = \"3.0.0\""));
        // bar should still have the old version
        assert!(content.contains("version = \"1.2.3\""));
    }

    // ==========================================================================
    // read_package_dependencies tests
    // ==========================================================================

    #[test]
    fn test_read_package_dependencies() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();

        // Create workspace with dependencies
        let root_manifest = r#"[workspace]
resolver = "2"
members = ["crates/app", "crates/lib"]

[workspace.package]
version = "1.0.0"

[workspace.dependencies]
lib = { path = "crates/lib", version = "1.0.0" }
"#;
        fs::write(root.join("Cargo.toml"), root_manifest).unwrap();

        // Create lib (no deps)
        fs::create_dir_all(root.join("crates/lib")).unwrap();
        let lib_manifest = r#"[package]
name = "lib"
version.workspace = true
"#;
        fs::write(root.join("crates/lib/Cargo.toml"), lib_manifest).unwrap();

        // Create app (depends on lib)
        fs::create_dir_all(root.join("crates/app")).unwrap();
        let app_manifest = r#"[package]
name = "app"
version.workspace = true

[dependencies]
lib = { workspace = true }
"#;
        fs::write(root.join("crates/app/Cargo.toml"), app_manifest).unwrap();

        let manifest = CargoManifest::new(&root);
        let deps = manifest.read_package_dependencies().unwrap();

        assert!(deps.contains_key("app"));
        assert!(deps.contains_key("lib"));
        assert!(deps.get("app").unwrap().contains(&"lib".to_string()));
        assert!(deps.get("lib").unwrap().is_empty());
    }

    // ==========================================================================
    // discover_members with glob patterns tests
    // ==========================================================================

    #[test]
    fn test_discover_members_glob_pattern() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();

        // Create workspace with glob pattern
        let root_manifest = r#"[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "1.0.0"
"#;
        fs::write(root.join("Cargo.toml"), root_manifest).unwrap();

        // Create member crates that match the glob
        fs::create_dir_all(root.join("crates/alpha")).unwrap();
        fs::create_dir_all(root.join("crates/beta")).unwrap();

        let alpha_manifest = r#"[package]
name = "alpha"
version.workspace = true
"#;
        fs::write(root.join("crates/alpha/Cargo.toml"), alpha_manifest).unwrap();

        let beta_manifest = r#"[package]
name = "beta"
version.workspace = true
"#;
        fs::write(root.join("crates/beta/Cargo.toml"), beta_manifest).unwrap();

        let manifest = CargoManifest::new(&root);
        let mut names = manifest.get_package_names().unwrap();
        names.sort();

        assert_eq!(names, vec!["alpha", "beta"]);
    }

    // ==========================================================================
    // new_version_value helper tests
    // ==========================================================================

    #[test]
    fn test_new_version_value() {
        let version = Version::new(1, 2, 3);
        let value = new_version_value(&version);
        assert_eq!(value.as_str(), Some("1.2.3"));
    }

    #[test]
    fn test_new_version_value_prerelease() {
        let version = "1.0.0-alpha.1".parse::<Version>().unwrap();
        let value = new_version_value(&version);
        assert_eq!(value.as_str(), Some("1.0.0-alpha.1"));
    }
}
