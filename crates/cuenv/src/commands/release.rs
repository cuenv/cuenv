//! Release management CLI commands.
//!
//! This module provides CLI commands for:
//! - `cuenv changeset add` - Create a new changeset
//! - `cuenv changeset status` - View pending changesets
//! - `cuenv changeset from-commits` - Generate changeset from conventional commits
//! - `cuenv release version` - Calculate and apply version bumps
//! - `cuenv release publish` - Publish workspace packages to crates.io in dependency order

mod binaries;
mod prepare;

pub use binaries::{ReleaseBinariesOptions, ReleaseBinariesPhase, execute_release_binaries};
pub use prepare::{PackageBumpInfo, ReleasePrepareOptions, execute_release_prepare};

use cuenv_release::{
    BumpType, CargoManifest, Changeset, ChangesetManager, CommitAnalyzer, CommitParser,
    CratesBackendConfig, CueBackendConfig, PackageChange, PublishPackage, PublishPlan,
    ReleaseConfig, TagType, VersionCalculator,
};
use cuengine::ModuleEvalOptions;
use serde::Deserialize;
use std::collections::HashSet;
use std::fmt::Write;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use toml::Value as TomlValue;

const DEFAULT_RELEASE_PACKAGE: &str = "cuenv";

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ProjectReleaseConfig {
    release: Option<ReleaseConfig>,
}

fn load_release_config(root: &Path) -> cuenv_core::Result<ReleaseConfig> {
    let config_dir = match super::env_file::find_env_file(root, DEFAULT_RELEASE_PACKAGE)? {
        super::env_file::EnvFileStatus::Match(path) => path,
        super::env_file::EnvFileStatus::Missing => return Ok(ReleaseConfig::default()),
        super::env_file::EnvFileStatus::PackageMismatch { found_package } => {
            return Err(cuenv_core::Error::configuration(format!(
                "env.cue package mismatch for release config: expected '{DEFAULT_RELEASE_PACKAGE}', found '{}'",
                found_package.unwrap_or_else(|| "unknown".to_string())
            )));
        }
    };

    let module_root = super::env_file::find_cue_module_root(&config_dir).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE module found (looking for cue.mod/) starting from: {}",
            config_dir.display()
        ))
    })?;
    super::module_version::ensure_compatible_module(&module_root)?;

    let options = ModuleEvalOptions {
        recursive: false,
        target_dir: Some(config_dir.to_string_lossy().to_string()),
        ..Default::default()
    };
    let raw = cuengine::evaluate_module(&module_root, DEFAULT_RELEASE_PACKAGE, Some(&options))
        .map_err(super::convert_engine_error)?;

    let value = raw
        .instances
        .get(".")
        .or_else(|| raw.instances.values().next())
        .ok_or_else(|| cuenv_core::Error::configuration("No env.cue instance found"))?;
    release_config_from_value(value)
}

fn release_config_from_value(value: &serde_json::Value) -> cuenv_core::Result<ReleaseConfig> {
    serde_json::from_value::<ProjectReleaseConfig>(value.clone())
        .map_err(|e| cuenv_core::Error::configuration(format!("Invalid release config: {e}")))
        .map(|project| project.release.unwrap_or_default())
}

/// Execute the `changeset add` command.
///
/// Creates a new changeset with the specified packages and bump types.
/// If no packages or summary are provided and stdin is a TTY, launches
/// an interactive picker.
///
/// # Errors
///
/// Returns an error if the changeset cannot be created or saved.
pub fn execute_changeset_add(
    path: &str,
    packages: &[(String, String)],
    summary: Option<&str>,
    description: Option<&str>,
) -> cuenv_core::Result<String> {
    let root = Path::new(path);

    // If no packages or summary provided and running interactively, launch picker
    if packages.is_empty()
        && summary.is_none()
        && std::io::IsTerminal::is_terminal(&std::io::stdin())
    {
        return execute_changeset_add_interactive(root);
    }

    // Validate we have the required args for non-interactive mode
    let summary = summary.ok_or_else(|| {
        cuenv_core::Error::configuration("Summary is required. Use -s or run interactively.")
    })?;

    if packages.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "At least one package must be specified. Use -P or run interactively.",
        ));
    }

    let manager = ChangesetManager::new(root);

    // Parse package changes
    let mut pkg_changes = Vec::new();
    for (name, bump_str) in packages {
        let bump = BumpType::parse(bump_str).map_err(|e| {
            cuenv_core::Error::configuration(format!("Invalid bump type for {name}: {e}"))
        })?;
        pkg_changes.push(PackageChange::new(name, bump));
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

/// Execute the interactive changeset add flow.
fn execute_changeset_add_interactive(root: &Path) -> cuenv_core::Result<String> {
    use super::changeset_picker::{ChangesetPickerResult, PackageInfo, run_changeset_picker};

    // Get package info from manifest
    let manifest = CargoManifest::new(root);
    let package_versions = manifest.read_package_versions().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to read package versions: {e}"))
    })?;

    let packages: Vec<PackageInfo> = package_versions
        .into_iter()
        .map(|(name, version)| PackageInfo {
            name,
            version: version.to_string(),
        })
        .collect();

    if packages.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "No packages found in workspace",
        ));
    }

    // Run the interactive picker
    let result = run_changeset_picker(packages)
        .map_err(|e| cuenv_core::Error::configuration(format!("Interactive picker failed: {e}")))?;

    match result {
        ChangesetPickerResult::Cancelled => Ok("Changeset creation cancelled.".to_string()),
        ChangesetPickerResult::Completed {
            packages: pkg_bumps,
            summary,
            description,
        } => {
            let manager = ChangesetManager::new(root);

            let pkg_changes: Vec<PackageChange> = pkg_bumps
                .into_iter()
                .map(|(name, bump)| PackageChange::new(name, bump))
                .collect();

            let changeset = Changeset::new(&summary, pkg_changes, description);

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
    }
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
    execute_changeset_status_with_format(path, crate::cli::OutputFormat::Text)
}

/// Execute the `changeset status` command with format option.
///
/// Lists all pending changesets and their accumulated bumps.
/// When format is JSON, returns structured JSON output suitable for CI parsing.
///
/// # Errors
///
/// Returns an error if changesets cannot be read.
pub fn execute_changeset_status_with_format(
    path: &str,
    format: crate::cli::OutputFormat,
) -> cuenv_core::Result<String> {
    let root = Path::new(path);
    let manager = ChangesetManager::new(root);

    let changesets = manager
        .list()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to read changesets: {e}")))?;

    // Get aggregated bumps
    let bumps = manager
        .get_package_bumps()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to aggregate bumps: {e}")))?;

    if format.is_json() {
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

        return serde_json::to_string_pretty(&output).map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to serialize JSON: {e}"))
        });
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
/// This function uses per-package versioning: it analyzes git diffs to determine
/// which packages each commit affects, and bumps only those packages. This enables
/// independent versioning in monorepos.
///
/// # Errors
///
/// Returns an error if commits cannot be parsed or changeset cannot be created.
pub fn execute_changeset_from_commits(
    path: &str,
    since_tag: Option<&str>,
) -> cuenv_core::Result<String> {
    let root = Path::new(path);
    let release_config = load_release_config(root)?;

    // Parse conventional commits
    let commits = CommitParser::parse_since_tag(
        root,
        since_tag,
        &release_config.git.tag_prefix,
        release_config.git.tag_type,
    )
    .map_err(|e| cuenv_core::Error::configuration(format!("Failed to parse commits: {e}")))?;

    if commits.is_empty() {
        return Ok("No conventional commits found since last tag.".to_string());
    }

    // Get package paths from manifest
    let manifest = CargoManifest::new(root);
    let package_paths = manifest.get_package_paths().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to read package paths: {e}"))
    })?;

    // Analyze commits per package
    let analyzer = CommitAnalyzer::new(root, package_paths.clone());
    let package_bumps = analyzer
        .calculate_bumps(&commits)
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to analyze commits: {e}")))?;

    if package_bumps.is_empty() {
        return Ok("No version-bumping commits found for any packages.\n\
             Use 'feat:' for features (minor) or 'fix:' for fixes (patch).\n\
             Note: Changes to root-level files don't affect package versions."
            .to_string());
    }

    // Create package changes only for affected packages
    let pkg_changes: Vec<PackageChange> = package_bumps
        .iter()
        .map(|(name, bump)| PackageChange::new(name, *bump))
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
    let _ = writeln!(output, "\nPackages affected:");

    // Sort for consistent output
    let mut sorted_bumps: Vec<_> = package_bumps.iter().collect();
    sorted_bumps.sort_by(|a, b| a.0.cmp(b.0));
    for (name, bump) in sorted_bumps {
        let _ = writeln!(output, "  • {name} ({bump})");
    }

    // Show packages not affected
    let affected_packages: std::collections::HashSet<_> = package_bumps.keys().collect();
    let all_packages: Vec<_> = package_paths
        .keys()
        .filter(|p| !affected_packages.contains(p))
        .collect();

    if !all_packages.is_empty() {
        let _ = writeln!(output, "\nPackages unchanged:");
        for name in all_packages {
            let _ = writeln!(output, "  • {name}");
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
pub fn execute_release_version(
    path: &str,
    dry_run: cuenv_core::DryRun,
) -> cuenv_core::Result<String> {
    let root = Path::new(path);
    let manager = ChangesetManager::new(root);
    let manifest = CargoManifest::new(root);
    let release_config = load_release_config(root)?;

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

    let calculator =
        VersionCalculator::new(current_versions.clone(), release_config.packages.clone());
    let new_versions = calculator.calculate(&bumps);

    let mut output = String::new();

    if dry_run.is_dry_run() {
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

    if !dry_run.is_dry_run() {
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

fn publish_to_crates_io(crate_dir: &Path) -> cuenv_core::Result<bool> {
    let manifest_path = crate_dir.join("Cargo.toml");
    let content = fs::read_to_string(&manifest_path).map_err(|e| {
        cuenv_core::Error::configuration(format!(
            "Failed to read crate manifest {}: {e}",
            manifest_path.display()
        ))
    })?;

    let doc: TomlValue = toml::from_str(&content).map_err(|e| {
        cuenv_core::Error::configuration(format!(
            "Failed to parse crate manifest {}: {e}",
            manifest_path.display()
        ))
    })?;

    let publish = doc.get("package").and_then(|p| p.get("publish"));

    match publish {
        Some(TomlValue::Boolean(false)) => Ok(false),
        Some(TomlValue::Array(arr)) => {
            if arr.is_empty() {
                return Ok(false);
            }
            Ok(arr
                .iter()
                .filter_map(TomlValue::as_str)
                .any(|v| v == "crates-io"))
        }
        _ => Ok(true),
    }
}

fn build_publish_plan(root: &Path) -> cuenv_core::Result<PublishPlan> {
    let manifest = CargoManifest::new(root);

    let package_paths = manifest.get_package_paths().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to read package paths: {e}"))
    })?;

    let package_versions = manifest.read_package_versions().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to read package versions: {e}"))
    })?;

    let package_deps = manifest.read_package_dependencies().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to read package dependencies: {e}"))
    })?;

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

    PublishPlan::from_packages(publish_packages).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to create publish plan: {e}"))
    })
}

fn publish_packages_to_crates_io(
    root: &Path,
    plan: &PublishPlan,
    publishable: &HashSet<String>,
    token_env: &str,
) -> cuenv_core::Result<()> {
    let registry_token = if token_env == "CARGO_REGISTRY_TOKEN" {
        None
    } else {
        std::env::var(token_env).ok()
    };

    for pkg in plan.iter() {
        if !publishable.contains(&pkg.name) {
            continue;
        }

        let mut command = Command::new("cargo");
        command
            .current_dir(root)
            .args(["publish", "-p"])
            .arg(&pkg.name)
            .arg("--locked");
        if let Some(token) = &registry_token {
            command.env("CARGO_REGISTRY_TOKEN", token);
        }

        let status = command
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|e| {
                cuenv_core::Error::execution_with_help(
                    format!("Failed to run 'cargo publish' for '{}': {e}", pkg.name),
                    format!(
                        "Ensure Rust/Cargo is available and {token_env} is set for crates.io publishing"
                    ),
                )
            })?;

        if !status.success() {
            return Err(cuenv_core::Error::execution_with_help(
                format!("'cargo publish' failed for '{}'", pkg.name),
                "Check the command output above (authentication, crate metadata, or version already published)",
            ));
        }
    }

    Ok(())
}

fn configured_publish_backends(
    config: &ReleaseConfig,
) -> (Option<CratesBackendConfig>, Option<CueBackendConfig>) {
    config.backends.as_ref().map_or_else(
        || (Some(CratesBackendConfig::default()), None),
        |backends| (backends.crates.clone(), backends.cue.clone()),
    )
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
    dry_run: cuenv_core::DryRun,
    format: OutputFormat,
) -> cuenv_core::Result<String> {
    let root = Path::new(path);
    let release_config = load_release_config(root)?;
    let (crates_config, cue_config) = configured_publish_backends(&release_config);
    if crates_config.is_none() && cue_config.is_none() {
        return Ok("No package release backends configured.".to_string());
    }
    if cue_config.is_some() && !dry_run.is_dry_run() {
        return Err(cuenv_core::Error::configuration(
            "CUE registry release publishing is not implemented yet",
        ));
    }

    let plan = build_publish_plan(root)?;

    // Extract package names in topological order
    let sorted_packages: Vec<String> = plan.iter().map(|p| p.name.clone()).collect();

    // Determine which packages are configured to publish.
    let mut publishable: HashSet<String> = HashSet::new();
    let mut skipped: HashSet<String> = HashSet::new();
    if crates_config.is_some() {
        for pkg in plan.iter() {
            let should_publish = publish_to_crates_io(&pkg.path)?;
            if should_publish {
                publishable.insert(pkg.name.clone());
            } else {
                skipped.insert(pkg.name.clone());
            }
        }
    }

    // Safety: don't allow publishing a crate that depends on an internal crate marked publish=false.
    for pkg in plan.iter() {
        if !publishable.contains(&pkg.name) {
            continue;
        }
        for dep in &pkg.dependencies {
            if skipped.contains(dep) {
                return Err(cuenv_core::Error::configuration(format!(
                    "Cannot publish '{}' because it depends on '{}' which is marked publish = false",
                    pkg.name, dep
                )));
            }
        }
    }

    if !dry_run.is_dry_run() && let Some(crates_config) = &crates_config {
        publish_packages_to_crates_io(root, &plan, &publishable, &crates_config.token_env)?;
    }

    match format {
        OutputFormat::Json => {
            let results = plan
                .iter()
                .map(|p| {
                    let status = if publishable.contains(&p.name) {
                        if dry_run.is_dry_run() {
                            "planned"
                        } else {
                            "published"
                        }
                    } else {
                        "skipped"
                    };

                    serde_json::json!({
                        "name": p.name,
                        "status": status,
                    })
                })
                .collect::<Vec<_>>();

            let json = serde_json::json!({
                "packages": sorted_packages,
                "results": results,
                "backends": {
                    "crates": crates_config.is_some(),
                    "cue": cue_config.is_some()
                },
                "dry_run": dry_run.is_dry_run()
            });
            serde_json::to_string_pretty(&json).map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to serialize JSON: {e}"))
            })
        }
        OutputFormat::Human => {
            let mut output = String::new();

            if dry_run.is_dry_run() {
                output.push_str("Dry run - no packages will be published.\n\n");
            }

            if dry_run.is_dry_run() {
                output.push_str("Publish plan (topologically sorted):\n\n");
            } else {
                output.push_str("Publish order (topologically sorted):\n\n");
            }

            for (i, pkg) in sorted_packages.iter().enumerate() {
                if publishable.contains(pkg) {
                    let _ = writeln!(output, "  {}. {pkg}", i + 1);
                } else {
                    let _ = writeln!(output, "  {}. {pkg} (skipped: publish disabled)", i + 1);
                }
            }

            if dry_run.is_dry_run() {
                output.push_str("\nDry run complete.\n");
            }
            if cue_config.is_some() {
                output.push_str("\nCUE registry publishing is configured but not implemented yet.\n");
            }

            Ok(output)
        }
    }
}

#[cfg(test)]
#[path = "release_tests.rs"]
mod tests;
