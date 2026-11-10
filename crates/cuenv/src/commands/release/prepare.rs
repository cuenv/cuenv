//! Release preparation command orchestration.

use cuenv_release::{
    BumpType, CargoManifest, ChangelogGenerator, Changeset, CommitAnalyzer, CommitParser,
    ConventionalCommit, PackageChange, ReleaseConfig, TagSelection, Version, VersionCalculator,
};
use std::collections::HashMap;
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};

/// Options for the `release prepare` command.
#[derive(Debug, Clone)]
pub struct ReleasePrepareOptions {
    /// Project root path.
    pub path: String,
    /// Git tag or ref to analyze commits from.
    pub since: Option<String>,
    /// Preview changes without applying.
    pub dry_run: cuenv_core::DryRun,
    /// Branch name for the release.
    pub branch: String,
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

struct ReleasePrepareAnalysis {
    root: PathBuf,
    commits: Vec<ConventionalCommit>,
    manifest: CargoManifest,
    release_config: ReleaseConfig,
    package_paths: HashMap<String, PathBuf>,
    new_versions: HashMap<String, Version>,
    bump_infos: Vec<PackageBumpInfo>,
    changelog_changesets: Vec<Changeset>,
}

enum ReleasePreparePlan {
    Ready(Box<ReleasePrepareAnalysis>),
    Nothing(String),
}

/// Execute the `release prepare` command.
///
/// This unified command orchestrates the release workflow:
/// 1. Analyze commits since the last tag
/// 2. Map commits to affected packages
/// 3. Calculate per-package version bumps
/// 4. Update Cargo.toml versions
/// 5. Generate/update CHANGELOG.md
/// 6. Create release branch and commit; pushing and opening the pull request
///    are left to the caller
///
/// # Errors
///
/// Returns an error if any step fails.
pub fn execute_release_prepare(opts: &ReleasePrepareOptions) -> cuenv_core::Result<String> {
    let analysis = match analyze_release_prepare(opts)? {
        ReleasePreparePlan::Ready(analysis) => analysis,
        ReleasePreparePlan::Nothing(message) => return Ok(message),
    };

    let mut output = render_release_prepare_summary(&analysis);
    if opts.dry_run.is_dry_run() {
        let _ = writeln!(output, "[DRY RUN] No changes applied.");
        let _ = writeln!(output, "\nTo apply changes, run without --dry-run");
        return Ok(output);
    }

    apply_release_prepare_versions(&analysis, &mut output)?;
    commit_and_publish_release_prepare(opts, &analysis, &mut output)?;

    let _ = writeln!(output, "\nRelease preparation complete!");
    Ok(output)
}

fn analyze_release_prepare(opts: &ReleasePrepareOptions) -> cuenv_core::Result<ReleasePreparePlan> {
    let root = Path::new(&opts.path).canonicalize().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to resolve path '{}': {e}", opts.path))
    })?;
    let release_config = super::implementation::load_release_config(&root)?;

    let commits = CommitParser::parse_since_tag(
        &root,
        TagSelection {
            since_tag: opts.since.as_deref(),
            tag_prefix: &release_config.git.tag_prefix,
            tag_type: release_config.git.tag_type,
        },
    )
    .map_err(|e| cuenv_core::Error::configuration(format!("Failed to parse commits: {e}")))?;

    if commits.is_empty() {
        return Ok(ReleasePreparePlan::Nothing(
            "No conventional commits found since last tag. Nothing to release.".to_string(),
        ));
    }

    let manifest = CargoManifest::new(&root);
    let package_paths = manifest.package_paths().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to read package paths: {e}"))
    })?;
    let package_versions = manifest.read_package_versions().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to read package versions: {e}"))
    })?;

    let analyzer = CommitAnalyzer::new(&root, package_paths.clone());
    let package_bumps = analyzer
        .calculate_bumps(&commits)
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to analyze commits: {e}")))?;

    if package_bumps.is_empty() {
        return Ok(ReleasePreparePlan::Nothing(
            "No packages affected by commits. Nothing to release.".to_string(),
        ));
    }

    if package_bumps.values().all(|b| *b == BumpType::None) {
        return Ok(ReleasePreparePlan::Nothing(
            "No version-bumping changes found. Nothing to release.".to_string(),
        ));
    }

    let calculator =
        VersionCalculator::new(package_versions.clone(), release_config.packages.clone());
    let new_versions = calculator.calculate(&package_bumps);

    let mut bump_infos = Vec::new();
    for (pkg_name, new_version) in &new_versions {
        let current = package_versions.get(pkg_name).ok_or_else(|| {
            cuenv_core::Error::configuration(format!("No version found for package: {pkg_name}"))
        })?;
        let bump_type = package_bumps
            .get(pkg_name)
            .copied()
            .unwrap_or(BumpType::None)
            .to_string();
        bump_infos.push(PackageBumpInfo {
            name: pkg_name.clone(),
            current_version: current.to_string(),
            new_version: new_version.to_string(),
            bump_type,
        });
    }
    let changelog_changesets = vec![Changeset::new(
        format!("Release from {} commits", commits.len()),
        package_bumps
            .iter()
            .map(|(name, bump)| PackageChange::new(name, *bump))
            .collect(),
        Some(CommitParser::summarize(&commits)),
    )];

    Ok(ReleasePreparePlan::Ready(Box::new(
        ReleasePrepareAnalysis {
            root,
            commits,
            manifest,
            release_config,
            package_paths,
            new_versions,
            bump_infos,
            changelog_changesets,
        },
    )))
}

fn render_release_prepare_summary(analysis: &ReleasePrepareAnalysis) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "Release Prepare Summary");
    let _ = writeln!(output, "=======================\n");
    let _ = writeln!(output, "Commits analyzed: {}", analysis.commits.len());
    let _ = writeln!(output, "Packages affected: {}\n", analysis.bump_infos.len());
    let _ = writeln!(
        output,
        "Changelog path: {}\n",
        analysis.release_config.changelog.path
    );

    let _ = writeln!(output, "Version Bumps:");
    let _ = writeln!(output, "{:-<60}", "");
    let _ = writeln!(output, "{:<30} {:>12} {:>12}", "Package", "Current", "New");
    let _ = writeln!(output, "{:-<60}", "");
    for info in &analysis.bump_infos {
        let _ = writeln!(
            output,
            "{:<30} {:>12} {:>12}",
            info.name, info.current_version, info.new_version
        );
    }
    let _ = writeln!(output, "{:-<60}\n", "");
    output
}

fn apply_release_prepare_versions(
    analysis: &ReleasePrepareAnalysis,
    output: &mut String,
) -> cuenv_core::Result<()> {
    let _ = writeln!(output, "Updating package versions...");
    for info in &analysis.bump_infos {
        if let Some(pkg_path) = analysis.package_paths.get(&info.name) {
            let manifest_path = pkg_path.join("Cargo.toml");
            update_package_version(&manifest_path, &info.new_version)?;
        }
    }

    let workspace_manifest = analysis.root.join("Cargo.toml");
    if let Ok(content) = fs::read_to_string(&workspace_manifest)
        && content.contains("[workspace.package]")
        && content.contains("version =")
        && let Some(primary) = analysis.bump_infos.first()
        && let Some(new_ver) = analysis.new_versions.get(&primary.name)
    {
        analysis
            .manifest
            .update_workspace_version(new_ver)
            .map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to update workspace version: {e}"))
            })?;

        analysis
            .manifest
            .update_workspace_dependency_versions(&analysis.new_versions)
            .map_err(|e| {
                cuenv_core::Error::configuration(format!(
                    "Failed to update workspace dependency versions: {e}"
                ))
            })?;
    }

    update_release_prepare_changelogs(analysis, output)?;

    Ok(())
}

fn update_release_prepare_changelogs(
    analysis: &ReleasePrepareAnalysis,
    output: &mut String,
) -> cuenv_core::Result<()> {
    let changelog_config = analysis.release_config.changelog.clone();
    if !changelog_config.workspace && !changelog_config.per_package {
        return Ok(());
    }

    let generator = ChangelogGenerator::new(changelog_config);
    let _ = writeln!(output, "Updating changelogs...");

    if analysis.release_config.changelog.workspace
        && let Some(entry) = generator
            .generate_workspace_entry(&analysis.changelog_changesets, &analysis.new_versions)
    {
        generator
            .update_file(
                &analysis.root.join(&analysis.release_config.changelog.path),
                &entry,
            )
            .map_err(|e| {
                cuenv_core::Error::configuration(format!(
                    "Failed to update workspace changelog: {e}"
                ))
            })?;
    }

    if !analysis.release_config.changelog.per_package {
        return Ok(());
    }

    for (package, version) in &analysis.new_versions {
        let Some(package_root) = analysis.package_paths.get(package) else {
            continue;
        };
        let Some(entry) =
            generator.generate_entries(&analysis.changelog_changesets, package, version)
        else {
            continue;
        };
        let changelog_path = generator.changelog_path(package_root);
        generator
            .update_file(&changelog_path, &entry)
            .map_err(|e| {
                cuenv_core::Error::configuration(format!(
                    "Failed to update changelog for {package}: {e}"
                ))
            })?;
    }

    Ok(())
}

fn commit_and_publish_release_prepare(
    opts: &ReleasePrepareOptions,
    analysis: &ReleasePrepareAnalysis,
    output: &mut String,
) -> cuenv_core::Result<()> {
    let _ = writeln!(output, "Creating release branch '{}'...", opts.branch);
    let commit_msg = format!(
        "chore(release): prepare release\n\n{}",
        analysis
            .bump_infos
            .iter()
            .map(|i| format!("- {}: {} -> {}", i.name, i.current_version, i.new_version))
            .collect::<Vec<_>>()
            .join("\n")
    );
    create_branch_and_commit(&analysis.root, &opts.branch, &commit_msg)?;

    let _ = writeln!(
        output,
        "\nCommitted the release to branch '{}'. Push it and open a pull request to continue.",
        opts.branch
    );

    Ok(())
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

/// Create a new branch, stage all working-tree changes, and commit them using gix.
///
/// This replaces the previous `git checkout -b`, `git add -A`, and `git commit` shell calls
/// with native gix operations. The approach:
/// 1. Discover the repository from the given path
/// 2. Build a new tree by diffing the worktree against the HEAD tree
///    - Handles tracked file modifications, deletions, and mode changes
///    - Walks worktree for new untracked files (matching `git add -A` semantics)
///    - Correctly handles symlinks by storing link targets, not dereferenced content
/// 3. Create the branch reference pointing to the current HEAD
/// 4. Point HEAD at the new branch (symbolic ref)
/// 5. Create a commit on HEAD with the new tree
fn create_branch_and_commit(root: &Path, branch: &str, message: &str) -> cuenv_core::Result<()> {
    use gix::object::tree::EntryKind;
    use gix::refs::transaction::PreviousValue;

    let repo = gix::discover(root).map_err(|e| {
        cuenv_core::Error::execution_with_help(
            format!("Failed to discover git repository: {e}"),
            "Ensure you are inside a valid git repository",
        )
    })?;

    let head_commit = repo.head_commit().map_err(|e| {
        cuenv_core::Error::execution_with_help(
            format!("Failed to resolve HEAD commit: {e}"),
            "Ensure the repository has at least one commit",
        )
    })?;
    let head_id = head_commit.id;
    let head_tree = head_commit.tree().map_err(|e| {
        cuenv_core::Error::execution_with_help(
            format!("Failed to read HEAD tree: {e}"),
            "Repository may be corrupted",
        )
    })?;

    // Build a new tree by updating blobs for all modified tracked files
    let workdir = repo
        .workdir()
        .ok_or_else(|| cuenv_core::Error::configuration("Cannot operate in a bare repository"))?;

    let mut editor = repo.edit_tree(head_tree.id).map_err(|e| {
        cuenv_core::Error::execution_with_help(
            format!("Failed to create tree editor: {e}"),
            "Repository may be corrupted",
        )
    })?;

    // Read the current index to find all tracked files, then check for modifications
    let index = repo.open_index().map_err(|e| {
        cuenv_core::Error::execution_with_help(
            format!("Failed to open index: {e}"),
            "Repository index may be missing or corrupted",
        )
    })?;

    // Collect tracked file paths for new-file detection later
    let mut tracked_paths: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();

    for entry in index.entries() {
        let rel_path = entry.path(&index);
        let rel_path_os = gix::path::from_bstr(rel_path);
        let abs_path = workdir.join(&rel_path_os);
        tracked_paths.insert(rel_path_os.to_path_buf());

        let path_str = String::from_utf8_lossy(rel_path);

        // Use lstat (no symlink follow) to get file metadata
        let fs_metadata = match gix::index::fs::Metadata::from_path_no_follow(&abs_path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // File was deleted — remove from tree
                editor.remove(path_str.as_ref()).map_err(|e| {
                    cuenv_core::Error::execution_with_help(
                        format!("Failed to remove tree entry for '{path_str}': {e}"),
                        "Tree editing failed while removing deleted files",
                    )
                })?;
                continue;
            }
            Err(e) => {
                return Err(cuenv_core::Error::execution_with_help(
                    format!(
                        "Failed to read file metadata for '{}': {e}",
                        abs_path.display()
                    ),
                    "Ensure the file is accessible before running release prepare",
                ));
            }
        };

        // Skip unchanged files using stat comparison (avoids reading+hashing every file)
        if let Ok(fs_stat) = gix::index::entry::Stat::from_fs(&fs_metadata)
            && entry
                .stat
                .matches(&fs_stat, gix::index::entry::stat::Options::default())
        {
            continue;
        }

        let metadata = abs_path.symlink_metadata().map_err(|e| {
            cuenv_core::Error::execution_with_help(
                format!(
                    "Failed to read file metadata for '{}': {e}",
                    abs_path.display()
                ),
                "Ensure the file is accessible before running release prepare",
            )
        })?;

        // Read content: for symlinks, store the link target path; for regular files, read bytes
        let content = if metadata.is_symlink() {
            let target = fs::read_link(&abs_path).map_err(|e| {
                cuenv_core::Error::execution_with_help(
                    format!("Failed to read symlink '{}': {e}", abs_path.display()),
                    "Ensure the symlink is readable",
                )
            })?;
            target.as_os_str().as_encoded_bytes().to_vec()
        } else {
            fs::read(&abs_path).map_err(|e| {
                cuenv_core::Error::execution_with_help(
                    format!("Failed to read file '{}': {e}", abs_path.display()),
                    "Ensure the file is readable before running release prepare",
                )
            })?
        };

        let blob_id = repo.write_blob(&content).map_err(|e| {
            cuenv_core::Error::execution_with_help(
                format!("Failed to write blob: {e}"),
                "Object database may be corrupted",
            )
        })?;

        // Determine the correct entry kind from the worktree metadata
        let kind = if metadata.is_symlink() {
            EntryKind::Link
        } else if is_executable(&metadata) {
            EntryKind::BlobExecutable
        } else {
            EntryKind::Blob
        };

        editor
            .upsert(path_str.as_ref(), kind, blob_id)
            .map_err(|e| {
                cuenv_core::Error::execution_with_help(
                    format!("Failed to update tree entry: {e}"),
                    "Tree editing failed",
                )
            })?;
    }

    // Walk the worktree for new (untracked) files to match `git add -A` semantics.
    // Uses ignore::WalkBuilder which respects .gitignore rules.
    for dir_entry in ignore::WalkBuilder::new(workdir)
        .hidden(false)
        .build()
        .flatten()
    {
        let path = dir_entry.path();
        if !path.is_file() && !path.is_symlink() {
            continue;
        }
        let Ok(rel_path) = path.strip_prefix(workdir) else {
            continue;
        };
        if tracked_paths.contains(rel_path) {
            continue;
        }
        // Skip .git directory entries
        if rel_path.starts_with(".git") {
            continue;
        }

        let metadata = path.symlink_metadata().map_err(|e| {
            cuenv_core::Error::execution_with_help(
                format!("Failed to read metadata for '{}': {e}", path.display()),
                "Ensure the file is accessible",
            )
        })?;

        let content = if metadata.is_symlink() {
            let target = fs::read_link(path).map_err(|e| {
                cuenv_core::Error::execution_with_help(
                    format!("Failed to read symlink '{}': {e}", path.display()),
                    "Ensure the symlink is readable",
                )
            })?;
            target.as_os_str().as_encoded_bytes().to_vec()
        } else {
            fs::read(path).map_err(|e| {
                cuenv_core::Error::execution_with_help(
                    format!("Failed to read file '{}': {e}", path.display()),
                    "Ensure the file is readable",
                )
            })?
        };

        let blob_id = repo.write_blob(&content).map_err(|e| {
            cuenv_core::Error::execution_with_help(
                format!("Failed to write blob: {e}"),
                "Object database may be corrupted",
            )
        })?;

        let kind = if metadata.is_symlink() {
            EntryKind::Link
        } else if is_executable(&metadata) {
            EntryKind::BlobExecutable
        } else {
            EntryKind::Blob
        };

        let rel_str = rel_path.to_string_lossy();
        editor
            .upsert(rel_str.as_ref(), kind, blob_id)
            .map_err(|e| {
                cuenv_core::Error::execution_with_help(
                    format!("Failed to add new file '{rel_str}' to tree: {e}"),
                    "Tree editing failed",
                )
            })?;
    }

    let new_tree_id = editor.write().map_err(|e| {
        cuenv_core::Error::execution_with_help(
            format!("Failed to write tree: {e}"),
            "Object database may be corrupted",
        )
    })?;

    // Create the branch reference pointing to the current HEAD commit
    let branch_ref = format!("refs/heads/{branch}");
    repo.reference(
        branch_ref.as_str(),
        head_id,
        PreviousValue::MustNotExist,
        format!("branch: Created {branch}"),
    )
    .map_err(|e| {
        cuenv_core::Error::execution_with_help(
            format!("Failed to create branch '{branch}': {e}"),
            "Check that the branch name is valid and does not already exist",
        )
    })?;

    // Point HEAD at the new branch (equivalent to git checkout)
    repo.edit_reference(gix::refs::transaction::RefEdit {
        change: gix::refs::transaction::Change::Update {
            log: gix::refs::transaction::LogChange::default(),
            expected: PreviousValue::Any,
            new: gix::refs::Target::Symbolic(branch_ref.try_into().map_err(
                |e: gix::validate::reference::name::Error| {
                    cuenv_core::Error::execution_with_help(
                        format!("Invalid branch name '{branch}': {e}"),
                        "Use a valid git branch name",
                    )
                },
            )?),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e: gix::validate::reference::name::Error| {
                cuenv_core::Error::execution_with_help(
                    format!("Failed to resolve HEAD: {e}"),
                    "Repository may be corrupted",
                )
            })?,
        deref: false,
    })
    .map_err(|e| {
        cuenv_core::Error::execution_with_help(
            format!("Failed to update HEAD to branch '{branch}': {e}"),
            "Reference transaction failed",
        )
    })?;

    // Create the commit on HEAD (which now points to the new branch)
    repo.commit("HEAD", message, new_tree_id, [head_id])
        .map_err(|e| {
            cuenv_core::Error::execution_with_help(
                format!("Failed to create commit: {e}"),
                "Ensure git user.name and user.email are configured",
            )
        })?;

    // Rewrite the index from the new commit's tree so `git status` is clean.
    let stale_index = |error: &dyn std::fmt::Display| {
        cuenv_core::Error::execution_with_help(
            format!("Failed to refresh index after commit: {error}"),
            "The commit was created successfully but the index may be stale",
        )
    };
    repo.index_from_tree(&new_tree_id)
        .map_err(|error| stale_index(&error))?
        .write(gix::index::write::Options::default())
        .map_err(|error| stale_index(&error))?;

    Ok(())
}

/// Check if file metadata indicates the executable bit is set.
#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    false
}
