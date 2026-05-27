//! Cargo workspace member discovery used by lockfile parsing.

use crate::error::{Error, Result};
use cargo_toml::{Error as CargoManifestError, Manifest};
use glob::{Pattern, glob};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub(super) type WorkspaceMembers = HashMap<String, PathBuf>;

pub(super) fn load_workspace_members(cargo_toml_path: &Path) -> Result<WorkspaceMembers> {
    let mut manifest = Manifest::from_path(cargo_toml_path)
        .map_err(|err| map_manifest_error(cargo_toml_path, err))?;

    let workspace_root = cargo_toml_path
        .parent()
        .ok_or_else(|| Error::LockfileParseFailed {
            path: cargo_toml_path.to_path_buf(),
            message: "Workspace root could not be determined".to_string(),
        })?;

    let mut members = WorkspaceMembers::new();

    if let Some(workspace) = manifest.workspace.as_ref() {
        collect_members(
            workspace_root,
            &workspace.members,
            &workspace.exclude,
            &mut members,
        )?;
        collect_members(
            workspace_root,
            &workspace.default_members,
            &workspace.exclude,
            &mut members,
        )?;
    } else if let Some(package) = manifest.package.take() {
        members.insert(package.name, PathBuf::from("."));
    }

    if members.is_empty() {
        return Err(Error::LockfileParseFailed {
            path: cargo_toml_path.to_path_buf(),
            message: "No workspace members or package declared in Cargo.toml".to_string(),
        });
    }

    Ok(members)
}

fn collect_members(
    workspace_root: &Path,
    member_patterns: &[String],
    exclude_patterns: &[String],
    members: &mut WorkspaceMembers,
) -> Result<()> {
    for pattern in member_patterns {
        if contains_glob(pattern) {
            collect_glob_members(workspace_root, pattern, exclude_patterns, members)?;
        } else {
            collect_member_dir(
                workspace_root,
                &workspace_root.join(pattern),
                exclude_patterns,
                members,
            )?;
        }
    }

    Ok(())
}

fn collect_glob_members(
    workspace_root: &Path,
    pattern: &str,
    exclude_patterns: &[String],
    members: &mut WorkspaceMembers,
) -> Result<()> {
    let glob_pattern = workspace_root.join(pattern);
    let glob_str = glob_pattern
        .to_str()
        .ok_or_else(|| Error::LockfileParseFailed {
            path: workspace_root.to_path_buf(),
            message: format!("Invalid UTF-8 in glob pattern: {pattern}"),
        })?;

    let entries = glob(glob_str).map_err(|err| Error::LockfileParseFailed {
        path: workspace_root.to_path_buf(),
        message: format!("Invalid glob pattern '{pattern}': {err}"),
    })?;

    for entry in entries {
        let member_dir = entry.map_err(|err| Error::LockfileParseFailed {
            path: workspace_root.to_path_buf(),
            message: format!("Glob error for pattern '{pattern}': {err}"),
        })?;

        if member_dir.is_dir() {
            collect_member_dir(workspace_root, &member_dir, exclude_patterns, members)?;
        }
    }

    Ok(())
}

fn collect_member_dir(
    workspace_root: &Path,
    member_dir: &Path,
    exclude_patterns: &[String],
    members: &mut WorkspaceMembers,
) -> Result<()> {
    if should_exclude(workspace_root, member_dir, exclude_patterns) {
        return Ok(());
    }

    process_member_dir(workspace_root, member_dir, members)
}

fn should_exclude(workspace_root: &Path, member_dir: &Path, exclude_patterns: &[String]) -> bool {
    let Ok(relative_path) = member_dir.strip_prefix(workspace_root) else {
        return false;
    };

    let Some(path_str) = relative_path.to_str() else {
        return false;
    };

    exclude_patterns
        .iter()
        .any(|pattern| path_matches(pattern, path_str))
}

fn path_matches(pattern: &str, path: &str) -> bool {
    if contains_glob(pattern) {
        Pattern::new(pattern).is_ok_and(|compiled| compiled.matches(path))
    } else {
        path == pattern
    }
}

fn process_member_dir(
    workspace_root: &Path,
    member_dir: &Path,
    members: &mut WorkspaceMembers,
) -> Result<()> {
    let manifest_path = member_dir.join("Cargo.toml");

    if !manifest_path.is_file() {
        return Ok(());
    }

    let mut manifest = Manifest::from_path(&manifest_path)
        .map_err(|err| map_manifest_error(&manifest_path, err))?;

    if let Some(package) = manifest.package.take() {
        let relative_path = member_dir
            .strip_prefix(workspace_root)
            .map_or_else(|_| member_dir.to_path_buf(), PathBuf::from);
        members.entry(package.name).or_insert(relative_path);
    }

    Ok(())
}

fn contains_glob(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?') || pattern.contains('[')
}

fn map_manifest_error(path: &Path, err: CargoManifestError) -> Error {
    match err {
        CargoManifestError::Parse(source) => Error::Toml {
            source: *source,
            path: Some(path.to_path_buf()),
        },
        CargoManifestError::Io(source) => Error::Io {
            source,
            path: Some(path.to_path_buf()),
            operation: "reading Cargo manifest".to_string(),
        },
        CargoManifestError::Workspace(inner) => {
            let (err, _) = *inner;
            Error::LockfileParseFailed {
                path: path.to_path_buf(),
                message: format!("workspace manifest error: {err}"),
            }
        }
        CargoManifestError::WorkspaceIntegrity(message) => Error::LockfileParseFailed {
            path: path.to_path_buf(),
            message,
        },
        CargoManifestError::InheritedUnknownValue => Error::LockfileParseFailed {
            path: path.to_path_buf(),
            message: "workspace manifest uses inherited values that have not been resolved"
                .to_string(),
        },
        CargoManifestError::Other(message) => Error::LockfileParseFailed {
            path: path.to_path_buf(),
            message: message.to_string(),
        },
        _ => Error::LockfileParseFailed {
            path: path.to_path_buf(),
            message: "unknown manifest error".to_string(),
        },
    }
}
