//! Release management CLI commands.
//!
//! This module provides CLI commands for:
//! - `cuenv changeset add` - Create a new changeset
//! - `cuenv changeset status` - View pending changesets
//! - `cuenv changeset from-commits` - Generate changeset from conventional commits
//! - `cuenv release version` - Calculate and apply version bumps
//! - `cuenv release publish` - Publish packages in topological order

use cuenv_release::{
    BumpType, CargoManifest, Changeset, ChangesetManager, CommitParser, PackageChange,
    PublishPackage, PublishPlan, ReleasePackagesConfig, VersionCalculator,
};
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

/// Status output for JSON mode
#[derive(Debug, serde::Serialize)]
pub struct ChangesetStatusOutput {
    /// Number of pending changesets
    pub count: usize,
    /// Whether there are pending changesets
    pub has_pending: bool,
    /// List of changeset summaries
    pub changesets: Vec<ChangesetSummary>,
    /// Aggregated bumps per package
    pub aggregated_bumps: std::collections::HashMap<String, String>,
}

/// Summary of a single changeset for JSON output
#[derive(Debug, serde::Serialize)]
pub struct ChangesetSummary {
    /// Changeset ID
    pub id: String,
    /// Summary description
    pub summary: String,
    /// Packages affected
    pub packages: Vec<PackageBumpSummary>,
}

/// Package bump info for JSON output
#[derive(Debug, serde::Serialize)]
pub struct PackageBumpSummary {
    /// Package name
    pub name: String,
    /// Bump type
    pub bump: String,
}

/// Execute the `changeset status` command.
///
/// Lists all pending changesets and their accumulated bumps.
/// This is a convenience wrapper that defaults to human-readable output.
///
/// # Errors
///
/// Returns an error if changesets cannot be read.
#[cfg(test)]
pub fn execute_changeset_status(path: &str) -> cuenv_core::Result<String> {
    execute_changeset_status_with_format(path, false)
}

/// Execute the `changeset status` command with format option.
///
/// Lists all pending changesets and their accumulated bumps.
/// When `json` is true, returns structured JSON output suitable for CI parsing.
///
/// # Errors
///
/// Returns an error if changesets cannot be read.
pub fn execute_changeset_status_with_format(path: &str, json: bool) -> cuenv_core::Result<String> {
    let root = Path::new(path);
    let manager = ChangesetManager::new(root);

    let changesets = manager
        .list()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to read changesets: {e}")))?;

    // Get aggregated bumps
    let bumps = manager
        .get_package_bumps()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to aggregate bumps: {e}")))?;

    if json {
        let output = ChangesetStatusOutput {
            count: changesets.len(),
            has_pending: !changesets.is_empty(),
            changesets: changesets
                .iter()
                .map(|cs| ChangesetSummary {
                    id: cs.id.clone(),
                    summary: cs.summary.clone(),
                    packages: cs
                        .packages
                        .iter()
                        .map(|pkg| PackageBumpSummary {
                            name: pkg.name.clone(),
                            bump: pkg.bump.to_string(),
                        })
                        .collect(),
                })
                .collect(),
            aggregated_bumps: bumps
                .iter()
                .map(|(k, v)| (k.clone(), v.to_string()))
                .collect(),
        };

        return serde_json::to_string_pretty(&output)
            .map_err(|e| cuenv_core::Error::configuration(format!("Failed to serialize JSON: {e}")));
    }

    // Human-readable output
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

/// Execute the `changeset from-commits` command.
///
/// Parses conventional commits since the last tag and creates a changeset.
///
/// This function applies a workspace-wide version bump strategy: it calculates the
/// maximum bump type from all conventional commits and applies it to ALL packages
/// in the workspace. This is intentional behavior for unified versioning across
/// the workspace. For per-package versioning, use `changeset add` to manually
/// specify version bumps for individual packages.
///
/// # Errors
///
/// Returns an error if commits cannot be parsed or changeset cannot be created.
pub fn execute_changeset_from_commits(
    path: &str,
    since_tag: Option<&str>,
) -> cuenv_core::Result<String> {
    let root = Path::new(path);

    // Parse conventional commits
    let commits = CommitParser::parse_since_tag(root, since_tag)
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to parse commits: {e}")))?;

    if commits.is_empty() {
        return Ok("No conventional commits found since last tag.".to_string());
    }

    // Calculate aggregate bump type (workspace-wide)
    let bump = CommitParser::aggregate_bump(&commits);
    if bump == BumpType::None {
        return Ok(
            "No version-bumping commits found (only chore, docs, etc.).\n\
             Use 'feat:' for features (minor) or 'fix:' for fixes (patch)."
                .to_string(),
        );
    }

    // Get package names from manifest
    let manifest = CargoManifest::new(root);
    let package_names = manifest.get_package_names().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to read package names: {e}"))
    })?;

    // Apply the aggregate bump to all workspace packages
    let pkg_changes: Vec<PackageChange> = package_names
        .iter()
        .map(|name| PackageChange::new(name, bump))
        .collect();

    // Generate summary from commits
    let summary = CommitParser::summarize(&commits);

    // Create changeset
    let manager = ChangesetManager::new(root);
    let changeset = Changeset::new(
        format!("Release from {} commits", commits.len()),
        pkg_changes,
        Some(summary),
    );

    let changeset_path = manager.add(&changeset).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to create changeset: {e}"))
    })?;

    let mut output = String::new();
    let _ = writeln!(
        output,
        "Created changeset from {} conventional commit(s)",
        commits.len()
    );
    let _ = writeln!(output, "  Path: {}", changeset_path.display());
    let _ = writeln!(output, "  ID: {}", changeset.id);
    let _ = writeln!(output, "  Bump type: {bump}");
    let _ = writeln!(output, "\nPackages affected:");
    for name in &package_names {
        let _ = writeln!(output, "  • {name}");
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
    let manifest = CargoManifest::new(root);

    let changesets = manager
        .list()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to read changesets: {e}")))?;

    if changesets.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "No changesets found. Run 'cuenv changeset add' first.",
        ));
    }

    // Read current versions from manifests
    let current_versions = manifest.read_package_versions().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to read package versions: {e}"))
    })?;

    // Get bumps from changesets
    let bumps = manager
        .get_package_bumps()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to aggregate bumps: {e}")))?;

    // Calculate new versions - for a workspace with shared versions, all packages share the same version
    let config = ReleasePackagesConfig::default();
    let calculator = VersionCalculator::new(current_versions.clone(), config);
    let new_versions = calculator.calculate(&bumps);

    let mut output = String::new();

    if dry_run {
        output.push_str("Dry run - no changes will be made.\n\n");
    }

    output.push_str("Version changes:\n\n");

    // Find the max new version (for workspace version)
    let max_new_version = new_versions.values().max().cloned();

    for (pkg, new_version) in &new_versions {
        let current = current_versions
            .get(pkg)
            .map_or("0.0.0".to_string(), std::string::ToString::to_string);
        let _ = writeln!(output, "  {pkg}: {current} -> {new_version}");
    }

    if !dry_run {
        // Update the workspace version
        if let Some(new_version) = max_new_version {
            manifest
                .update_workspace_version(&new_version)
                .map_err(|e| {
                    cuenv_core::Error::configuration(format!(
                        "Failed to update workspace version: {e}"
                    ))
                })?;

            // Update workspace dependency versions
            manifest
                .update_workspace_dependency_versions(&new_versions)
                .map_err(|e| {
                    cuenv_core::Error::configuration(format!(
                        "Failed to update dependency versions: {e}"
                    ))
                })?;

            // Clear consumed changesets
            manager.clear().map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to clear changesets: {e}"))
            })?;

            output.push_str("\nManifest files updated successfully.\n");
            output.push_str("Changesets have been consumed.\n");
        }
    }

    Ok(output)
}

/// Output format for release publish command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Human-readable output
    Human,
    /// JSON output for CI consumption
    Json,
}

/// Execute the `release publish` command.
///
/// Returns the topological order for publishing packages.
///
/// # Errors
///
/// Returns an error if package order cannot be determined.
pub fn execute_release_publish(
    path: &str,
    dry_run: bool,
    format: OutputFormat,
) -> cuenv_core::Result<String> {
    let root = Path::new(path);
    let manifest = CargoManifest::new(root);

    // Get package names, paths, and versions
    let package_paths = manifest.get_package_paths().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to read package paths: {e}"))
    })?;

    let package_versions = manifest.read_package_versions().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to read package versions: {e}"))
    })?;

    // Get package dependencies
    let package_deps = manifest.read_package_dependencies().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to read package dependencies: {e}"))
    })?;

    // Build PublishPackage list
    let mut publish_packages = Vec::new();
    for (name, path) in &package_paths {
        let version = package_versions.get(name).ok_or_else(|| {
            cuenv_core::Error::configuration(format!("No version found for package: {name}"))
        })?;
        let dependencies = package_deps.get(name).cloned().unwrap_or_default();

        publish_packages.push(PublishPackage {
            name: name.clone(),
            path: path.clone(),
            version: version.clone(),
            dependencies,
        });
    }

    // Create publish plan with topological ordering
    let plan = PublishPlan::from_packages(publish_packages).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to create publish plan: {e}"))
    })?;

    // Extract package names in topological order
    let sorted_packages: Vec<String> = plan.iter().map(|p| p.name.clone()).collect();

    match format {
        OutputFormat::Json => {
            let json = serde_json::json!({
                "packages": sorted_packages,
                "dry_run": dry_run
            });
            serde_json::to_string_pretty(&json)
                .map_err(|e| cuenv_core::Error::configuration(format!("Failed to serialize JSON: {e}")))
        }
        OutputFormat::Human => {
            let mut output = String::new();

            if dry_run {
                output.push_str("Dry run - no packages will be published.\n\n");
            }

            output.push_str("Publish order (topologically sorted):\n\n");
            for (i, pkg) in sorted_packages.iter().enumerate() {
                let _ = writeln!(output, "  {}. {pkg}", i + 1);
            }

            output.push_str(
                "\nTo publish, create a GitHub Release which triggers the release workflow.\n",
            );

            Ok(output)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_workspace(temp: &TempDir) -> String {
        let root = temp.path();

        // Create root Cargo.toml
        let root_manifest = r#"
[workspace]
resolver = "2"
members = ["crates/foo", "crates/bar"]

[workspace.package]
version = "1.0.0"
edition = "2021"

[workspace.dependencies]
foo = { path = "crates/foo", version = "1.0.0" }
bar = { path = "crates/bar", version = "1.0.0" }
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

        root.to_string_lossy().to_string()
    }

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
        let path = create_test_workspace(&temp);

        let result = execute_release_version(&path, true);
        assert!(result.is_err());
    }

    #[test]
    fn test_release_version_dry_run() {
        let temp = TempDir::new().unwrap();
        let path = create_test_workspace(&temp);

        // Add a changeset first
        let packages = vec![("foo".to_string(), "minor".to_string())];
        execute_changeset_add(&path, &packages, "Feature", None).unwrap();

        let result = execute_release_version(&path, true);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Dry run"));
        assert!(output.contains("Version changes"));
    }

    #[test]
    fn test_release_version_apply() {
        let temp = TempDir::new().unwrap();
        let path = create_test_workspace(&temp);

        // Add a changeset
        let packages = vec![("foo".to_string(), "minor".to_string())];
        execute_changeset_add(&path, &packages, "Feature", None).unwrap();

        // Apply version changes
        let result = execute_release_version(&path, false);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Manifest files updated"));
        assert!(output.contains("Changesets have been consumed"));

        // Verify version was updated
        let manifest = CargoManifest::new(Path::new(&path));
        let version = manifest.read_workspace_version().unwrap();
        assert_eq!(version.to_string(), "1.1.0");
    }

    #[test]
    fn test_release_publish_dry_run_human() {
        let temp = TempDir::new().unwrap();
        let path = create_test_workspace(&temp);

        let result = execute_release_publish(&path, true, OutputFormat::Human);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Dry run"));
        assert!(output.contains("Publish order"));
    }

    #[test]
    fn test_release_publish_json() {
        let temp = TempDir::new().unwrap();
        let path = create_test_workspace(&temp);

        let result = execute_release_publish(&path, false, OutputFormat::Json);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("\"packages\""));
        assert!(output.contains("bar"));
        assert!(output.contains("foo"));
    }

    /// Helper function to initialize and configure a git repository for testing
    fn init_git_repo(path: &str) {
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .unwrap();

        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(path)
            .output()
            .unwrap();

        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(path)
            .output()
            .unwrap();
    }

    /// Helper function to create a git commit
    fn create_git_commit(path: &str, message: &str) {
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .unwrap();

        std::process::Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(path)
            .output()
            .unwrap();
    }

    #[test]
    fn test_changeset_from_commits_no_git_repo() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().to_str().unwrap();

        // Should fail because there's no git repository
        let result = execute_changeset_from_commits(path, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_changeset_from_commits_with_workspace() {
        let temp = TempDir::new().unwrap();
        let path = create_test_workspace(&temp);

        init_git_repo(&path);
        create_git_commit(&path, "feat: add new feature");

        // Now test the function
        let result = execute_changeset_from_commits(&path, None);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Created changeset"));
        assert!(output.contains("conventional commit"));
    }

    #[test]
    fn test_changeset_from_commits_no_version_bumps() {
        let temp = TempDir::new().unwrap();
        let path = create_test_workspace(&temp);

        init_git_repo(&path);
        create_git_commit(&path, "chore: update deps");

        // Should return message about no version-bumping commits
        let result = execute_changeset_from_commits(&path, None);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("No version-bumping commits"));
    }

    #[test]
    fn test_changeset_from_commits_with_since_tag() {
        let temp = TempDir::new().unwrap();
        let path = create_test_workspace(&temp);

        init_git_repo(&path);
        create_git_commit(&path, "fix: initial fix");

        // Create a tag
        std::process::Command::new("git")
            .args(["tag", "v0.1.0"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Create a second commit (after tag) - this should be picked up
        let new_file = std::path::Path::new(&path).join("new-file.txt");
        std::fs::write(new_file, "content").unwrap();
        create_git_commit(&path, "feat: new feature after tag");

        // Test with since_tag - should only process commits after the tag
        let result = execute_changeset_from_commits(&path, Some("v0.1.0"));
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Created changeset"));
        assert!(output.contains("conventional commit"));
        // Should have created changeset from 1 commit (the one after the tag)
        assert!(output.contains("1 conventional commit"));
    }

    #[test]
    fn test_changeset_from_commits_with_nonexistent_tag() {
        let temp = TempDir::new().unwrap();
        let path = create_test_workspace(&temp);

        init_git_repo(&path);
        create_git_commit(&path, "feat: new feature");

        // Test with non-existent tag - should return error
        let result = execute_changeset_from_commits(&path, Some("v0.1.0"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Tag 'v0.1.0' not found"));
    }
}
