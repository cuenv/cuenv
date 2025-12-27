//! Release management CLI commands.
//!
//! This module provides CLI commands for:
//! - `cuenv changeset add` - Create a new changeset
//! - `cuenv changeset status` - View pending changesets
//! - `cuenv changeset from-commits` - Generate changeset from conventional commits
//! - `cuenv release version` - Calculate and apply version bumps
//! - `cuenv release publish` - Publish workspace packages to crates.io in dependency order

use cuenv_release::{
    BumpType, CargoManifest, Changeset, ChangesetManager, CommitAnalyzer, CommitParser,
    PackageChange, PublishPackage, PublishPlan, ReleasePackagesConfig, TagType, Version,
    VersionCalculator,
};
use std::collections::HashSet;
use std::fmt::Write;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use toml::Value as TomlValue;

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

    // Parse conventional commits
    // TODO: Load tag_prefix and tag_type from project's release config (env.cue)
    let commits = CommitParser::parse_since_tag(root, since_tag, "", TagType::Semver)
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
) -> cuenv_core::Result<()> {
    for pkg in plan.iter() {
        if !publishable.contains(&pkg.name) {
            continue;
        }

        let status = Command::new("cargo")
            .current_dir(root)
            .arg("publish")
            .arg("-p")
            .arg(&pkg.name)
            .arg("--locked")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|e| {
                cuenv_core::Error::execution_with_help(
                    format!("Failed to run 'cargo publish' for '{}': {e}", pkg.name),
                    "Ensure Rust/Cargo is available and CARGO_REGISTRY_TOKEN is set for crates.io publishing",
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
    let plan = build_publish_plan(root)?;

    // Extract package names in topological order
    let sorted_packages: Vec<String> = plan.iter().map(|p| p.name.clone()).collect();

    // Determine which packages are configured to publish.
    let mut publishable: HashSet<String> = HashSet::new();
    let mut skipped: HashSet<String> = HashSet::new();
    for pkg in plan.iter() {
        let should_publish = publish_to_crates_io(&pkg.path)?;
        if should_publish {
            publishable.insert(pkg.name.clone());
        } else {
            skipped.insert(pkg.name.clone());
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

    if !dry_run {
        publish_packages_to_crates_io(root, &plan, &publishable)?;
    }

    match format {
        OutputFormat::Json => {
            let results = plan
                .iter()
                .map(|p| {
                    let status = if publishable.contains(&p.name) {
                        if dry_run { "planned" } else { "published" }
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
                "dry_run": dry_run
            });
            serde_json::to_string_pretty(&json).map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to serialize JSON: {e}"))
            })
        }
        OutputFormat::Human => {
            let mut output = String::new();

            if dry_run {
                output.push_str("Dry run - no packages will be published.\n\n");
            }

            if dry_run {
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

            if dry_run {
                output.push_str("\nDry run complete.\n");
            }

            Ok(output)
        }
    }
}

/// Release phase to execute.
#[derive(Debug, Clone, Copy, Default)]
pub enum ReleaseBinariesPhase {
    /// Build binaries only.
    Build,
    /// Package binaries only.
    Package,
    /// Publish only (requires existing artifacts).
    Publish,
    /// Full pipeline: build, package, publish.
    #[default]
    Full,
}

/// Options for the `release binaries` command.
#[derive(Debug, Clone, Default)]
pub struct ReleaseBinariesOptions {
    /// Project root path.
    pub path: String,
    /// Dry run mode (no actual publishing).
    pub dry_run: bool,
    /// Filter to specific backends.
    pub backends: Option<Vec<String>>,
    /// Release phase to execute.
    pub phase: ReleaseBinariesPhase,
    /// Target platforms to build for.
    pub targets: Option<Vec<String>>,
    /// Version to release (auto-detected from Cargo.toml if not provided).
    pub version: Option<String>,
}

impl ReleaseBinariesOptions {
    /// Creates new options with the given path.
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            ..Default::default()
        }
    }

    /// Sets dry run mode.
    #[must_use]
    pub const fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Sets the backends filter.
    #[must_use]
    pub fn with_backends(mut self, backends: Option<Vec<String>>) -> Self {
        self.backends = backends;
        self
    }

    /// Sets the release phase.
    #[must_use]
    pub const fn with_phase(mut self, phase: ReleaseBinariesPhase) -> Self {
        self.phase = phase;
        self
    }

    /// Sets the target platforms.
    #[must_use]
    pub fn with_targets(mut self, targets: Option<Vec<String>>) -> Self {
        self.targets = targets;
        self
    }

    /// Sets the version.
    #[must_use]
    pub fn with_version(mut self, version: Option<String>) -> Self {
        self.version = version;
        self
    }
}

/// Execute the `release binaries` command.
///
/// Builds, packages, and publishes binary releases to configured backends.
///
/// # Errors
///
/// Returns an error if the release process fails.
#[allow(clippy::format_push_string, clippy::too_many_lines)]
pub async fn execute_release_binaries(opts: ReleaseBinariesOptions) -> cuenv_core::Result<String> {
    use cuenv_release::{
        CargoManifest, OrchestratorConfig, ReleaseOrchestrator, ReleasePhase, Target,
    };
    use std::path::Path;

    let root = Path::new(&opts.path);

    // Get version from Cargo.toml if not provided
    let release_version = if let Some(v) = opts.version {
        v
    } else {
        let manifest = CargoManifest::new(root);
        manifest
            .read_workspace_version()
            .map_err(|e| cuenv_core::Error::configuration(format!("Failed to read version: {e}")))?
            .to_string()
    };

    // Get binary name from Cargo.toml (use first package name or workspace name)
    let manifest = CargoManifest::new(root);
    let binary_name = manifest
        .get_package_names()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to read packages: {e}")))?
        .into_iter()
        .next()
        .ok_or_else(|| cuenv_core::Error::configuration("No packages found in workspace"))?;

    // Parse targets
    let release_targets: Vec<Target> = if let Some(target_strs) = opts.targets {
        target_strs
            .iter()
            .map(|t| {
                t.parse::<Target>().map_err(|e| {
                    cuenv_core::Error::configuration(format!("Invalid target '{t}': {e}"))
                })
            })
            .collect::<cuenv_core::Result<Vec<_>>>()?
    } else {
        vec![Target::LinuxX64, Target::LinuxArm64, Target::DarwinArm64]
    };

    // Build config
    let config = OrchestratorConfig::new(&binary_name, &release_version)
        .with_targets(release_targets)
        .with_output_dir("target/release-artifacts")
        .with_dry_run(opts.dry_run);

    // Determine phase
    let phase = match opts.phase {
        ReleaseBinariesPhase::Build => ReleasePhase::Build,
        ReleaseBinariesPhase::Package => ReleasePhase::Package,
        ReleaseBinariesPhase::Publish => ReleasePhase::Publish,
        ReleaseBinariesPhase::Full => ReleasePhase::Full,
    };

    // Create backends
    let mut backends: Vec<Box<dyn cuenv_release::ReleaseBackend>> = Vec::new();

    // Add GitHub Releases backend if available
    #[cfg(feature = "github")]
    {
        if let Ok(token) = std::env::var("GITHUB_TOKEN") {
            // Try to get repo info from git remote
            if let Some((owner, repo)) = get_github_repo_from_remote(root) {
                let config = cuenv_github::GitHubReleaseConfig::new(&owner, &repo, token);
                backends.push(Box::new(cuenv_github::GitHubReleaseBackend::new(config)));
            }
        }
    }

    // Add Homebrew backend if available
    #[cfg(feature = "homebrew")]
    {
        // Only add if token is available (indicates intent to publish)
        if std::env::var("HOMEBREW_TAP_TOKEN").is_ok() {
            // TODO: Load tap config from CUE release config
            // For now, use a sensible default based on project name
            let tap = format!("{binary_name}/homebrew-tap");
            let config = cuenv_homebrew::HomebrewConfig::new(&tap, &binary_name)
                .with_license("AGPL-3.0-or-later")
                .with_homepage(format!("https://github.com/{binary_name}"));
            backends.push(Box::new(cuenv_homebrew::HomebrewBackend::new(config)));
        }
    }

    // Apply backend filter if specified
    if let Some(ref filter) = opts.backends {
        let filter_lower: Vec<String> = filter.iter().map(|s| s.to_lowercase()).collect();
        backends.retain(|b| filter_lower.contains(&b.name().to_lowercase()));
    }

    // Create orchestrator with backends
    let orchestrator = ReleaseOrchestrator::new(config).with_backends(backends);

    // Run orchestrator
    let report = orchestrator
        .run(phase)
        .await
        .map_err(|e| cuenv_core::Error::configuration(format!("Release failed: {e}")))?;

    // Format output
    let mut output = String::new();

    if opts.dry_run {
        output.push_str("[dry-run] ");
    }

    output.push_str(&format!("Release {binary_name} v{release_version}\n"));
    output.push_str(&format!("Phase: {:?}\n", report.phase));

    if !report.artifacts.is_empty() {
        output.push_str("\nArtifacts:\n");
        for artifact in &report.artifacts {
            output.push_str(&format!(
                "  - {} ({})\n",
                artifact.archive_name, artifact.sha256
            ));
        }
    }

    if !report.backend_results.is_empty() {
        output.push_str("\nBackend results:\n");
        for result in &report.backend_results {
            let status = if result.success { "✓" } else { "✗" };
            output.push_str(&format!(
                "  {} {}: {}\n",
                status, result.backend, result.message
            ));
            if let Some(url) = &result.url {
                output.push_str(&format!("      URL: {url}\n"));
            }
        }
    }

    if report.success {
        output.push_str("\nRelease completed successfully.\n");
    } else {
        output.push_str("\nRelease completed with errors.\n");
    }

    Ok(output)
}

/// Gets the GitHub owner/repo from the git remote origin.
#[cfg(feature = "github")]
fn get_github_repo_from_remote(root: &std::path::Path) -> Option<(String, String)> {
    use std::process::Command;

    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(root)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_github_url(&url)
}

/// Parses a GitHub URL into (owner, repo).
#[cfg(feature = "github")]
fn parse_github_url(url: &str) -> Option<(String, String)> {
    // Handle SSH format: git@github.com:owner/repo.git
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        let path = rest.strip_suffix(".git").unwrap_or(rest);
        let (owner, repo) = path.split_once('/')?;
        return Some((owner.to_string(), repo.to_string()));
    }

    // Handle HTTPS format: https://github.com/owner/repo.git
    if let Some(rest) = url.strip_prefix("https://github.com/") {
        let path = rest.strip_suffix(".git").unwrap_or(rest);
        let (owner, repo) = path.split_once('/')?;
        return Some((owner.to_string(), repo.to_string()));
    }

    None
}

/// Options for the `release prepare` command.
#[derive(Debug, Clone)]
pub struct ReleasePrepareOptions {
    /// Project root path.
    pub path: String,
    /// Git tag or ref to analyze commits from.
    pub since: Option<String>,
    /// Preview changes without applying.
    pub dry_run: bool,
    /// Branch name for the release.
    pub branch: String,
    /// Skip creating the pull request.
    pub no_pr: bool,
}

/// Information about a package version bump.
#[derive(Debug, serde::Serialize)]
pub struct PackageBumpInfo {
    /// Package name.
    pub name: String,
    /// Current version.
    pub current_version: String,
    /// New version.
    pub new_version: String,
    /// Bump type.
    pub bump_type: String,
}

/// Execute the `release prepare` command.
///
/// This unified command orchestrates the release workflow:
/// 1. Analyze commits since the last tag
/// 2. Map commits to affected packages
/// 3. Calculate per-package version bumps
/// 4. Update Cargo.toml versions
/// 5. Generate/update CHANGELOG.md
/// 6. Create release branch, commit, and push
/// 7. Create PR via `gh` CLI
///
/// # Errors
///
/// Returns an error if any step fails.
#[allow(clippy::too_many_lines)]
pub fn execute_release_prepare(opts: &ReleasePrepareOptions) -> cuenv_core::Result<String> {
    let root = Path::new(&opts.path).canonicalize().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to resolve path '{}': {e}", &opts.path))
    })?;

    // Step 1: Parse commits since last tag
    // TODO: Load tag_prefix and tag_type from project's release config (env.cue)
    let commits = CommitParser::parse_since_tag(&root, opts.since.as_deref(), "", TagType::Semver)
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to parse commits: {e}")))?;

    if commits.is_empty() {
        return Ok("No conventional commits found since last tag. Nothing to release.".to_string());
    }

    // Step 2: Get workspace packages
    let manifest = CargoManifest::new(&root);
    let package_paths = manifest.get_package_paths().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to read package paths: {e}"))
    })?;
    let package_versions = manifest.read_package_versions().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to read package versions: {e}"))
    })?;

    // Step 3: Analyze which packages each commit affects
    let analyzer = CommitAnalyzer::new(&root, package_paths.clone());
    let package_bumps = analyzer
        .calculate_bumps(&commits)
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to analyze commits: {e}")))?;

    if package_bumps.is_empty() {
        return Ok("No packages affected by commits. Nothing to release.".to_string());
    }

    // Step 4: Calculate unified version for fixed (lockstep) versioning
    // All packages in the workspace share the same version.

    // Find max bump type across all affected packages
    let max_bump = package_bumps
        .values()
        .filter(|b| **b != BumpType::None)
        .max()
        .copied()
        .unwrap_or(BumpType::None);

    if max_bump == BumpType::None {
        return Ok("No version-bumping changes found. Nothing to release.".to_string());
    }

    // Find max current version across ALL workspace packages
    let max_current = package_versions
        .values()
        .max()
        .cloned()
        .ok_or_else(|| cuenv_core::Error::configuration("No packages found in workspace"))?;

    // Adjust for pre-1.0: breaking changes (Major) become Minor bumps
    let adjusted_bump = max_current.adjusted_bump_type(max_bump);
    let new_version = max_current.bump(adjusted_bump);

    // Apply same version to ALL packages (fixed/lockstep versioning)
    let mut bump_infos = Vec::new();
    for (pkg_name, current) in &package_versions {
        bump_infos.push(PackageBumpInfo {
            name: pkg_name.clone(),
            current_version: current.to_string(),
            new_version: new_version.to_string(),
            bump_type: adjusted_bump.to_string(),
        });
    }
    let new_versions: std::collections::HashMap<String, Version> = package_versions
        .keys()
        .map(|k| (k.clone(), new_version.clone()))
        .collect();

    // Build output
    let mut output = String::new();
    let _ = writeln!(output, "Release Prepare Summary");
    let _ = writeln!(output, "=======================\n");
    let _ = writeln!(output, "Commits analyzed: {}", commits.len());
    let _ = writeln!(output, "Packages affected: {}\n", bump_infos.len());

    let _ = writeln!(output, "Version Bumps:");
    let _ = writeln!(output, "{:-<60}", "");
    let _ = writeln!(output, "{:<30} {:>12} {:>12}", "Package", "Current", "New");
    let _ = writeln!(output, "{:-<60}", "");
    for info in &bump_infos {
        let _ = writeln!(
            output,
            "{:<30} {:>12} {:>12}",
            info.name, info.current_version, info.new_version
        );
    }
    let _ = writeln!(output, "{:-<60}\n", "");

    if opts.dry_run {
        let _ = writeln!(output, "[DRY RUN] No changes applied.");
        let _ = writeln!(output, "\nTo apply changes, run without --dry-run");
        return Ok(output);
    }

    // Step 5: Update Cargo.toml versions
    let _ = writeln!(output, "Updating package versions...");
    for info in &bump_infos {
        if let Some(pkg_path) = package_paths.get(&info.name) {
            let manifest_path = pkg_path.join("Cargo.toml");
            update_package_version(&manifest_path, &info.new_version)?;
        }
    }

    // Also update workspace version if present
    let workspace_manifest = root.join("Cargo.toml");
    if let Ok(content) = fs::read_to_string(&workspace_manifest)
        && content.contains("[workspace.package]")
        && content.contains("version =")
        && let Some(primary) = bump_infos.first()
        && let Some(new_ver) = new_versions.get(&primary.name)
    {
        manifest.update_workspace_version(new_ver).map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to update workspace version: {e}"))
        })?;

        // Also update workspace dependency versions
        manifest
            .update_workspace_dependency_versions(&new_versions)
            .map_err(|e| {
                cuenv_core::Error::configuration(format!(
                    "Failed to update workspace dependency versions: {e}"
                ))
            })?;
    }

    // Step 6: Create release branch
    let _ = writeln!(output, "Creating release branch '{}'...", opts.branch);
    run_git_command(&root, &["checkout", "-b", &opts.branch])?;

    // Step 7: Commit changes
    let _ = writeln!(output, "Committing version updates...");
    run_git_command(&root, &["add", "-A"])?;

    let commit_msg = format!(
        "chore(release): prepare release\n\n{}",
        bump_infos
            .iter()
            .map(|i| format!("- {}: {} -> {}", i.name, i.current_version, i.new_version))
            .collect::<Vec<_>>()
            .join("\n")
    );
    run_git_command(&root, &["commit", "-m", &commit_msg])?;

    // Step 8: Push branch
    let _ = writeln!(output, "Pushing branch to origin...");
    run_git_command(&root, &["push", "-u", "origin", &opts.branch])?;

    // Step 9: Create PR
    if !opts.no_pr {
        let _ = writeln!(output, "Creating pull request...");
        let pr_body = generate_pr_body(&bump_infos, &commits);
        let pr_title = format!(
            "chore(release): prepare release {}",
            bump_infos
                .first()
                .map_or("next", |i| i.new_version.as_str())
        );

        match create_pull_request(&root, &pr_title, &pr_body) {
            Ok(pr_url) => {
                let _ = writeln!(output, "\nPull request created: {pr_url}");
            }
            Err(e) => {
                let _ = writeln!(output, "\nWarning: Failed to create PR: {e}");
                let _ = writeln!(output, "You can create the PR manually.");
            }
        }
    }

    let _ = writeln!(output, "\nRelease preparation complete!");
    Ok(output)
}

/// Update a package's Cargo.toml with new version.
fn update_package_version(manifest_path: &Path, new_version: &str) -> cuenv_core::Result<()> {
    let content = fs::read_to_string(manifest_path).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to read {}: {e}", manifest_path.display()))
    })?;

    // Simple regex-free version update
    let mut new_content = String::new();
    let mut in_package = false;
    let mut version_updated = false;

    for line in content.lines() {
        if line.trim() == "[package]" {
            in_package = true;
        } else if line.starts_with('[') {
            in_package = false;
        }

        if in_package && line.trim().starts_with("version") && !version_updated {
            // Check if it's workspace reference
            if line.contains("workspace = true") {
                new_content.push_str(line);
            } else {
                let _ = write!(new_content, "version = \"{new_version}\"");
                version_updated = true;
            }
        } else {
            new_content.push_str(line);
        }
        new_content.push('\n');
    }

    fs::write(manifest_path, new_content).map_err(|e| {
        cuenv_core::Error::configuration(format!(
            "Failed to write {}: {e}",
            manifest_path.display()
        ))
    })?;

    Ok(())
}

/// Run a git command.
fn run_git_command(root: &Path, args: &[&str]) -> cuenv_core::Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| {
            cuenv_core::Error::execution_with_help(
                format!("Failed to run git {}: {e}", args.join(" ")),
                "Ensure git is installed and available in PATH",
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(cuenv_core::Error::execution_with_help(
            format!("git {} failed: {stderr}", args.join(" ")),
            "Check the error message above",
        ));
    }

    Ok(())
}

/// Generate PR body from bump info and commits.
fn generate_pr_body(
    bumps: &[PackageBumpInfo],
    commits: &[cuenv_release::ConventionalCommit],
) -> String {
    let mut body = String::new();

    body.push_str("## Summary\n\n");

    // Version table
    body.push_str("| Package | Current | New | Bump |\n");
    body.push_str("|---------|---------|-----|------|\n");
    for info in bumps {
        let _ = writeln!(
            body,
            "| {} | {} | {} | {} |",
            info.name, info.current_version, info.new_version, info.bump_type
        );
    }

    body.push_str("\n## Commits\n\n");

    // Group commits by type
    let mut features: Vec<&cuenv_release::ConventionalCommit> = Vec::new();
    let mut fixes: Vec<&cuenv_release::ConventionalCommit> = Vec::new();
    let mut others: Vec<&cuenv_release::ConventionalCommit> = Vec::new();

    for commit in commits {
        match commit.commit_type.as_str() {
            "feat" => features.push(commit),
            "fix" => fixes.push(commit),
            _ => others.push(commit),
        }
    }

    if !features.is_empty() {
        body.push_str("### Features\n\n");
        for c in &features {
            let scope = c
                .scope
                .as_ref()
                .map_or(String::new(), |s| format!("**{s}**: "));
            let _ = writeln!(body, "- {}{}", scope, c.description);
        }
        body.push('\n');
    }

    if !fixes.is_empty() {
        body.push_str("### Bug Fixes\n\n");
        for c in &fixes {
            let scope = c
                .scope
                .as_ref()
                .map_or(String::new(), |s| format!("**{s}**: "));
            let _ = writeln!(body, "- {}{}", scope, c.description);
        }
        body.push('\n');
    }

    if !others.is_empty() {
        body.push_str("### Other Changes\n\n");
        for c in &others {
            let scope = c
                .scope
                .as_ref()
                .map_or(String::new(), |s| format!("**{s}**: "));
            let _ = writeln!(body, "- {}{}", scope, c.description);
        }
    }

    body.push_str("\n---\n\n🤖 Generated with [cuenv](https://github.com/cuenv/cuenv)\n");

    body
}

/// Create a pull request using gh CLI.
fn create_pull_request(root: &Path, title: &str, body: &str) -> cuenv_core::Result<String> {
    let output = Command::new("gh")
        .args(["pr", "create", "--title", title, "--body", body])
        .current_dir(root)
        .output()
        .map_err(|e| {
            cuenv_core::Error::execution_with_help(
                format!("Failed to run gh pr create: {e}"),
                "Ensure gh CLI is installed and authenticated (gh auth login)",
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(cuenv_core::Error::execution_with_help(
            format!("gh pr create failed: {stderr}"),
            "Ensure gh CLI is authenticated and repository has a remote origin",
        ));
    }

    let pr_url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(pr_url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn create_test_workspace(temp: &TempDir) -> String {
        let root = temp.path();

        // Create root Cargo.toml
        let root_manifest = r#"[workspace]
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

        root.to_string_lossy().to_string()
    }

    #[test]
    fn test_changeset_add() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().to_str().unwrap();

        let packages = vec![("my-pkg".to_string(), "minor".to_string())];

        let result =
            execute_changeset_add(path, &packages, Some("Add feature"), Some("Details here"));

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

        let result = execute_changeset_add(path, &packages, Some("Test"), None);
        assert!(result.is_err());
    }

    #[test]
    fn test_changeset_add_no_packages() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().to_str().unwrap();

        let packages: Vec<(String, String)> = vec![];

        let result = execute_changeset_add(path, &packages, Some("Test"), None);
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
        execute_changeset_add(path, &packages, Some("Add feature"), None).unwrap();

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
        execute_changeset_add(&path, &packages, Some("Feature"), None).unwrap();

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
        execute_changeset_add(&path, &packages, Some("Feature"), None).unwrap();

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
        assert!(output.contains("Publish plan"));
    }

    #[test]
    fn test_release_publish_json() {
        let temp = TempDir::new().unwrap();
        let path = create_test_workspace(&temp);

        let result = execute_release_publish(&path, true, OutputFormat::Json);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("\"packages\""));
        assert!(output.contains("bar"));
        assert!(output.contains("foo"));
    }

    /// Helper function to initialize and configure a git repository for testing
    fn init_git_repo(path: &str) {
        let out = Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let out = Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git config user.name failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let out = Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git config user.email failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Helper function to create a git commit
    fn create_git_commit(path: &str, message: &str) {
        let out = Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git add failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let out = Command::new("git")
            .args(["commit", "--no-gpg-sign", "-m", message])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
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
        // The file must be in a package directory for per-package analysis to detect it
        let new_file = std::path::Path::new(&path).join("crates/foo/new-feature.rs");
        std::fs::write(new_file, "// new feature").unwrap();
        create_git_commit(&path, "feat: new feature after tag");

        // Test with since_tag - should only process commits after the tag
        let result = execute_changeset_from_commits(&path, Some("v0.1.0"));
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Created changeset"));
        assert!(output.contains("conventional commit"));
        // Should have created changeset from 1 commit (the one after the tag)
        assert!(output.contains("1 conventional commit"));
        // Only foo should be affected (not bar)
        assert!(output.contains("foo"));
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
