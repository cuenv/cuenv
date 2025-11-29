//! Release management CLI commands.
//!
//! This module provides CLI commands for:
//! - `cuenv changeset add` - Create a new changeset
//! - `cuenv changeset status` - View pending changesets
//! - `cuenv release version` - Calculate and apply version bumps
//! - `cuenv release publish` - Publish packages in topological order

use cuenv_release::{BumpType, Changeset, ChangesetManager, PackageChange, Version};
use std::fmt::Write;
use std::path::Path;

/// Execute the `changeset add` command.
///
/// Creates a new changeset with the specified packages and bump types.
///
/// # Errors
///
/// Returns an error if the changeset cannot be created or saved.
pub fn execute_changeset_add(
    path: &str,
    packages: &[(String, String)],
    summary: &str,
    description: Option<&str>,
) -> cuenv_core::Result<String> {
    let root = Path::new(path);
    let manager = ChangesetManager::new(root);

    // Parse package changes
    let mut pkg_changes = Vec::new();
    for (name, bump_str) in packages {
        let bump = BumpType::parse(bump_str).map_err(|e| {
            cuenv_core::Error::configuration(format!("Invalid bump type for {name}: {e}"))
        })?;
        pkg_changes.push(PackageChange::new(name, bump));
    }

    if pkg_changes.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "At least one package must be specified",
        ));
    }

    let changeset = Changeset::new(summary, pkg_changes, description.map(String::from));

    let changeset_path = manager.add(&changeset).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to create changeset: {e}"))
    })?;

    Ok(format!(
        "Created changeset: {}\n  ID: {}\n  Summary: {}",
        changeset_path.display(),
        changeset.id,
        changeset.summary
    ))
}

/// Execute the `changeset status` command.
///
/// Lists all pending changesets and their accumulated bumps.
///
/// # Errors
///
/// Returns an error if changesets cannot be read.
pub fn execute_changeset_status(path: &str) -> cuenv_core::Result<String> {
    let root = Path::new(path);
    let manager = ChangesetManager::new(root);

    let changesets = manager
        .list()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to read changesets: {e}")))?;

    if changesets.is_empty() {
        return Ok(
            "No pending changesets found.\n\nRun 'cuenv changeset add' to create one.".to_string(),
        );
    }

    let mut output = String::new();
    let _ = writeln!(output, "Found {} pending changeset(s):\n", changesets.len());

    for cs in &changesets {
        let _ = writeln!(output, "  {} - {}", cs.id, cs.summary);
        for pkg in &cs.packages {
            let _ = writeln!(output, "    • {} ({})", pkg.name, pkg.bump);
        }
        output.push('\n');
    }

    // Show aggregated bumps
    let bumps = manager
        .get_package_bumps()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to aggregate bumps: {e}")))?;

    if !bumps.is_empty() {
        output.push_str("Aggregated version bumps:\n\n");
        let mut sorted_bumps: Vec<_> = bumps.iter().collect();
        sorted_bumps.sort_by(|a, b| a.0.cmp(b.0));
        for (pkg, bump) in sorted_bumps {
            let _ = writeln!(output, "  {pkg}: {bump}");
        }
    }

    Ok(output)
}

/// Execute the `release version` command.
///
/// Calculates new versions based on changesets and optionally updates manifest files.
///
/// # Errors
///
/// Returns an error if version calculation fails.
pub fn execute_release_version(path: &str, dry_run: bool) -> cuenv_core::Result<String> {
    let root = Path::new(path);
    let manager = ChangesetManager::new(root);

    let changesets = manager
        .list()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to read changesets: {e}")))?;

    if changesets.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "No changesets found. Run 'cuenv changeset add' first.",
        ));
    }

    let bumps = manager
        .get_package_bumps()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to aggregate bumps: {e}")))?;

    let mut output = String::new();

    if dry_run {
        output.push_str("Dry run - no changes will be made.\n\n");
    }

    output.push_str("Version changes:\n\n");

    // For now, we just show what would happen
    // TODO: Read current versions from Cargo.toml/package.json
    for (pkg, bump) in &bumps {
        let current = Version::new(0, 1, 0); // Placeholder
        let new_version = current.bump(*bump);
        let _ = writeln!(output, "  {pkg}: {current} -> {new_version}");
    }

    if !dry_run {
        output.push_str("\nNote: Manifest updates not yet implemented.\n");
        output.push_str("Run with --dry-run to preview changes.\n");
    }

    Ok(output)
}

/// Execute the `release publish` command.
///
/// Publishes packages in topological dependency order.
///
/// # Errors
///
/// Returns an error if publishing fails.
#[allow(clippy::unnecessary_wraps)]
pub fn execute_release_publish(path: &str, dry_run: bool) -> cuenv_core::Result<String> {
    let _root = Path::new(path);

    let mut output = String::new();

    if dry_run {
        output.push_str("Dry run - no packages will be published.\n\n");
    }

    output.push_str("Release publish:\n\n");
    output.push_str("Note: Publish workflow not yet implemented.\n");
    output.push_str("This will execute 'publish' tasks in topological order.\n");

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_changeset_add() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().to_str().unwrap();

        let packages = vec![("my-pkg".to_string(), "minor".to_string())];

        let result = execute_changeset_add(path, &packages, "Add feature", Some("Details here"));

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Created changeset"));
        assert!(output.contains("Add feature"));
    }

    #[test]
    fn test_changeset_add_invalid_bump() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().to_str().unwrap();

        let packages = vec![("my-pkg".to_string(), "invalid".to_string())];

        let result = execute_changeset_add(path, &packages, "Test", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_changeset_add_no_packages() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().to_str().unwrap();

        let packages: Vec<(String, String)> = vec![];

        let result = execute_changeset_add(path, &packages, "Test", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_changeset_status_empty() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().to_str().unwrap();

        let result = execute_changeset_status(path);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("No pending changesets"));
    }

    #[test]
    fn test_changeset_status_with_changesets() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().to_str().unwrap();

        // First add a changeset
        let packages = vec![("pkg-a".to_string(), "minor".to_string())];
        execute_changeset_add(path, &packages, "Add feature", None).unwrap();

        // Then check status
        let result = execute_changeset_status(path);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("1 pending changeset"));
        assert!(output.contains("Add feature"));
        assert!(output.contains("pkg-a"));
    }

    #[test]
    fn test_release_version_no_changesets() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().to_str().unwrap();

        let result = execute_release_version(path, true);
        assert!(result.is_err());
    }

    #[test]
    fn test_release_version_dry_run() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().to_str().unwrap();

        // Add a changeset first
        let packages = vec![("pkg-a".to_string(), "minor".to_string())];
        execute_changeset_add(path, &packages, "Feature", None).unwrap();

        let result = execute_release_version(path, true);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Dry run"));
        assert!(output.contains("Version changes"));
    }

    #[test]
    fn test_release_publish_dry_run() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().to_str().unwrap();

        let result = execute_release_publish(path, true);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("Dry run"));
    }
}
