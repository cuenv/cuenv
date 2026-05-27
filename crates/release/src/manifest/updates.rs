use super::CargoManifest;
use crate::error::{Error, Result};
use crate::version::Version;
use std::collections::HashMap;
use std::fs;
use toml_edit::{Item, Value};

impl CargoManifest {
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

        let mut doc = content.parse::<toml_edit::DocumentMut>().map_err(|e| {
            Error::manifest(
                format!("Failed to parse root Cargo.toml: {e}"),
                Some(path.clone()),
            )
        })?;

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

        let mut doc = content.parse::<toml_edit::DocumentMut>().map_err(|e| {
            Error::manifest(
                format!("Failed to parse root Cargo.toml: {e}"),
                Some(path.clone()),
            )
        })?;

        if let Some(workspace) = doc.get_mut("workspace")
            && let Some(deps) = workspace.get_mut("dependencies")
            && let Item::Table(deps_table) = deps
        {
            for (pkg_name, version) in packages {
                if let Some(dep) = deps_table.get_mut(pkg_name) {
                    update_dependency_version(dep, version);
                }
            }
        }

        fs::write(&path, doc.to_string()).map_err(|e| {
            Error::manifest(format!("Failed to write root Cargo.toml: {e}"), Some(path))
        })?;

        Ok(())
    }
}

fn update_dependency_version(dep: &mut Item, version: &Version) {
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

/// Create a new TOML value for a version string.
pub(super) fn new_version_value(version: &Version) -> Value {
    Value::from(version.to_string())
}
