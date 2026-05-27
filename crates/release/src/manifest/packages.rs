use super::CargoManifest;
use crate::error::{Error, Result};
use crate::version::Version;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::Item;

struct MemberManifest {
    package_root: PathBuf,
    doc: toml::Value,
}

impl CargoManifest {
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
            let Some(member) = read_member_manifest(&member_path)? else {
                continue;
            };

            if let Some(name) = package_name(&member.doc) {
                paths_map.insert(name.to_string(), member.package_root);
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
            let Some(member) = read_member_manifest(&member_path)? else {
                continue;
            };

            if let Some(name) = package_name(&member.doc) {
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
            let Some(member) = read_member_manifest(&member_path)? else {
                continue;
            };

            let Some(package) = member.doc.get("package") else {
                continue;
            };

            let Some(name) = package.get("name").and_then(|n| n.as_str()) else {
                continue;
            };

            if let Some(version) = package_version(package, &workspace_version)? {
                versions.insert(name.to_string(), version);
            }
        }

        Ok(versions)
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
        let internal_packages = self.get_internal_package_names()?;
        let members = self.discover_members()?;
        let mut dependencies_map = HashMap::new();

        for member_path in members {
            let Some(member) = read_member_manifest(&member_path)? else {
                continue;
            };

            let Some(package) = member.doc.get("package") else {
                continue;
            };

            let Some(name) = package.get("name").and_then(|n| n.as_str()) else {
                continue;
            };

            let deps = workspace_internal_dependencies(&member.doc, &internal_packages);
            dependencies_map.insert(name.to_string(), deps);
        }

        Ok(dependencies_map)
    }

    /// Discover all workspace member directories.
    pub(super) fn discover_members(&self) -> Result<Vec<PathBuf>> {
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
                self.push_member_paths(pattern, &mut paths)?;
            }
        }

        Ok(paths)
    }

    fn push_member_paths(&self, pattern: &str, paths: &mut Vec<PathBuf>) -> Result<()> {
        if pattern.contains('*') {
            return self.push_globbed_member_paths(pattern, paths);
        }

        paths.push(self.root.join(pattern));
        Ok(())
    }

    fn push_globbed_member_paths(&self, pattern: &str, paths: &mut Vec<PathBuf>) -> Result<()> {
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

        Ok(())
    }

    /// Get the names of internal packages from `[workspace.dependencies]`.
    ///
    /// Internal packages are those defined with a `path` key.
    fn get_internal_package_names(&self) -> Result<HashSet<String>> {
        let doc = self.read_root_manifest()?;
        let mut internal_packages = HashSet::new();

        if let Some(workspace) = doc.get("workspace")
            && let Some(deps) = workspace.get("dependencies")
            && let Some(deps_table) = deps.as_table()
        {
            for (name, value) in deps_table {
                if dependency_has_path(value) {
                    internal_packages.insert(name.to_string());
                }
            }
        }

        Ok(internal_packages)
    }
}

fn read_member_manifest(member_path: &Path) -> Result<Option<MemberManifest>> {
    let manifest_path = member_path.join("Cargo.toml");
    if !manifest_path.exists() {
        return Ok(None);
    }

    let doc = read_toml_value(&manifest_path)?;

    Ok(Some(MemberManifest {
        package_root: member_path.to_path_buf(),
        doc,
    }))
}

fn read_toml_value(path: &Path) -> Result<toml::Value> {
    let content = fs::read_to_string(path).map_err(|e| {
        Error::manifest(
            format!("Failed to read {}: {e}", path.display()),
            Some(path.to_path_buf()),
        )
    })?;

    toml::from_str(&content).map_err(|e| {
        Error::manifest(
            format!("Failed to parse {}: {e}", path.display()),
            Some(path.to_path_buf()),
        )
    })
}

fn package_name(doc: &toml::Value) -> Option<&str> {
    doc.get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
}

fn package_version(package: &toml::Value, workspace_version: &Version) -> Result<Option<Version>> {
    if let Some(version_table) = package.get("version").and_then(|v| v.as_table()) {
        if version_table
            .get("workspace")
            .is_some_and(|w| w.as_bool() == Some(true))
        {
            return Ok(Some(workspace_version.clone()));
        }

        return Ok(None);
    }

    package
        .get("version")
        .and_then(|v| v.as_str())
        .map(str::parse::<Version>)
        .transpose()
}

fn workspace_internal_dependencies(
    member_doc: &toml::Value,
    internal_packages: &HashSet<String>,
) -> Vec<String> {
    member_doc
        .get("dependencies")
        .and_then(|d| d.as_table())
        .map(|dependencies| {
            dependencies
                .iter()
                .filter(|(dep_name, dep_value)| {
                    is_internal_dependency(dep_name, dep_value, internal_packages)
                })
                .map(|(dep_name, _)| dep_name.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn is_internal_dependency(
    dep_name: &str,
    dep_value: &toml::Value,
    internal_packages: &HashSet<String>,
) -> bool {
    let Some(dep_table) = dep_value.as_table() else {
        return false;
    };

    let is_workspace = dep_table.get("workspace") == Some(&toml::Value::Boolean(true));
    let has_path = dep_table.contains_key("path");

    has_path || (is_workspace && internal_packages.contains(dep_name))
}

fn dependency_has_path(value: &Item) -> bool {
    value
        .as_inline_table()
        .is_some_and(|inline| inline.contains_key("path"))
        || value
            .as_table()
            .is_some_and(|table| table.contains_key("path"))
}
