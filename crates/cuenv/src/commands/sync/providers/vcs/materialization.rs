//! VCS dependency checkout and materialization helpers.

use super::git::{Directory, Fetch, History};
use super::paths;
use super::paths::TempPath;
use super::vcs_sync_provider::{
    InstallOverlayDependencyRequest, MARKER_FILE, PrepareDependencyRequest,
    PrepareSubdirDependencyRequest,
};
use cuenv_core::lockfile::LockedVcsDependency;
use cuenv_core::manifest::VcsDependency;
use cuenv_core::{Error, Result};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

pub(super) fn prune_removed_vcs_dependencies(
    git_root: &Path,
    dependencies: &[LockedVcsDependency],
) -> Result<()> {
    for dependency in dependencies {
        let target = paths::validate_materialization_path(git_root, &dependency.path)?;
        if dependency.overlay {
            ensure_overlay_parent_target(&target)?;
            // Overlay deps never own the parent `path`; remove only the managed
            // children, leaving repo-local siblings intact.
            for (child, child_tree) in &dependency.children {
                let child_target = target.join(child);
                if fs::symlink_metadata(&child_target).is_ok() {
                    let child_previous = child_locked(dependency, child, child_tree);
                    ensure_replaceable_target(&child_target, Some(&child_previous))?;
                    fs::remove_dir_all(&child_target)
                        .map_err(|e| Error::configuration(e.to_string()))?;
                }
            }
            continue;
        }
        if fs::symlink_metadata(&target).is_ok() {
            ensure_replaceable_target(&target, Some(dependency))?;
            fs::remove_dir_all(&target).map_err(|e| Error::configuration(e.to_string()))?;
        }
    }
    Ok(())
}

pub(super) fn should_update(name: &str, update: Option<&[String]>) -> bool {
    match update {
        None => false,
        Some([]) => true,
        Some(names) => names.iter().any(|candidate| candidate == name),
    }
}

pub(super) fn locked_matches(locked: Option<&LockedVcsDependency>, spec: &VcsDependency) -> bool {
    locked.is_some_and(|locked| {
        locked.url == spec.url
            && locked.reference == spec.reference
            && locked.vendor == spec.vendor
            && locked.path == spec.path
            && locked.subdir == spec.subdir
            && locked.overlay == spec.overlay
    })
}

pub(super) fn resolve_dependency(
    cache_root: &Path,
    name: &str,
    spec: &VcsDependency,
) -> Result<LockedVcsDependency> {
    validate_git_input("url", &spec.url)?;
    validate_git_input("reference", &spec.reference)?;
    let normalized_subdir = spec
        .subdir
        .as_deref()
        .map(paths::validate_subdir)
        .transpose()?;
    if spec.overlay {
        if spec.vendor {
            return Err(Error::configuration(format!(
                "VCS dependency '{name}': overlay is incompatible with vendor"
            )));
        }
        if normalized_subdir.is_none() {
            return Err(Error::configuration(format!(
                "VCS dependency '{name}': overlay requires a subdir"
            )));
        }
    }
    let cache_path = cache_root.join(name);
    fs::create_dir_all(cache_root).map_err(|e| Error::configuration(e.to_string()))?;
    paths::ensure_managed_internal_path(cache_root, &cache_path)?;
    let repository = if cache_path.exists() {
        ensure_git_dir(&cache_path)?;
        super::git::open(&cache_path)?
    } else {
        super::git::clone_without_checkout(&spec.url, &cache_path)?
    };
    let commit = super::git::fetch(
        &repository,
        Fetch {
            url: &spec.url,
            reference: &spec.reference,
            history: History::Full,
        },
    )?;
    let tree = super::git::tree_of(&repository, &commit)?;

    let subtree = if let Some(ref subdir) = normalized_subdir {
        let entry = super::git::entry_at(&repository, &commit, subdir)?.ok_or_else(|| {
            Error::configuration(format!(
                "VCS dependency '{}': subdir '{}' not found at reference '{}'",
                name, subdir, spec.reference
            ))
        })?;
        if entry.kind != "tree" {
            return Err(Error::configuration(format!(
                "VCS dependency '{}': subdir '{}' must be a tree (directory), got {} at reference '{}'",
                name, subdir, entry.kind, spec.reference
            )));
        }
        Some(entry.id)
    } else {
        None
    };

    let children = match (normalized_subdir.as_deref(), subtree.as_deref()) {
        // Guarded above: overlay implies a subdir, so this branch always runs.
        (Some(subdir), Some(subtree)) if spec.overlay => {
            list_subtree_children(&repository, subtree, subdir)?
        }
        _ => BTreeMap::new(),
    };

    Ok(LockedVcsDependency {
        url: spec.url.clone(),
        reference: spec.reference.clone(),
        commit,
        tree,
        vendor: spec.vendor,
        path: spec.path.clone(),
        subdir: normalized_subdir,
        subtree,
        overlay: spec.overlay,
        children,
    })
}

/// Synthesize a per-child locked entry from an overlay parent. Each child is a
/// self-contained snapshot materialization at `parent.path/child` whose marker
/// records the parent commit and whose snapshot tree is the child's own tree.
fn child_locked(
    parent: &LockedVcsDependency,
    child: &str,
    child_tree: &str,
) -> LockedVcsDependency {
    let subdir = parent
        .subdir
        .as_deref()
        .map(|subdir| format!("{}/{}", subdir.trim_end_matches('/'), child));
    LockedVcsDependency {
        url: parent.url.clone(),
        reference: parent.reference.clone(),
        commit: parent.commit.clone(),
        tree: parent.tree.clone(),
        vendor: false,
        path: format!("{}/{}", parent.path.trim_end_matches('/'), child),
        subdir,
        subtree: Some(child_tree.to_string()),
        overlay: false,
        children: BTreeMap::new(),
    }
}

/// Enumerate the immediate children of a git tree as `name -> tree SHA`.
/// Overlay only supports directory children; files and submodules are rejected.
fn list_subtree_children(
    repository: &gix::Repository,
    tree: &str,
    context: &str,
) -> Result<BTreeMap<String, String>> {
    let mut children = BTreeMap::new();
    for entry in super::git::entries(repository, tree)? {
        let name = entry.name;
        match entry.kind {
            "tree" => {
                paths::validate_overlay_child_name(&name, context)?;
                children.insert(name, entry.id);
            }
            "blob" => {
                return Err(Error::configuration(format!(
                    "overlay subtree '{context}' contains file '{name}'; overlay supports directory children only"
                )));
            }
            _ => {
                return Err(Error::configuration(format!(
                    "overlay subtree '{context}' contains submodule '{name}'; overlay supports directory children only"
                )));
            }
        }
    }
    if children.is_empty() {
        return Err(Error::configuration(format!(
            "overlay subtree '{context}' has no directory children"
        )));
    }
    Ok(children)
}

pub(super) fn prepare_dependency(request: PrepareDependencyRequest<'_>) -> Result<TempPath> {
    let PrepareDependencyRequest {
        temp_root,
        target,
        locked,
        previous,
    } = request;
    validate_git_input("url", &locked.url)?;
    validate_git_input("reference", &locked.reference)?;
    validate_git_input("commit", &locked.commit)?;
    ensure_replaceable_target(target, previous)?;
    fs::create_dir_all(temp_root).map_err(|e| Error::configuration(e.to_string()))?;
    if let Some(ref subdir) = locked.subdir {
        return prepare_subdir_dependency(PrepareSubdirDependencyRequest {
            temp_root,
            target,
            locked,
            subdir,
        });
    }
    let temp_target = paths::temporary_target_path(temp_root, target, "tmp")?;
    let temp_guard = TempPath::new(temp_target);
    let temp_target = temp_guard.path();
    if locked.vendor {
        // A vendored dependency is its files alone: fetch the one commit into
        // a scratch repository and write its tree out.
        let scratch = paths::temporary_target_path(temp_root, target, "clone")?;
        let scratch_guard = TempPath::new(scratch);
        let repository = super::git::init(scratch_guard.path())?;
        super::git::fetch(
            &repository,
            Fetch {
                url: &locked.url,
                reference: &locked.commit,
                history: History::TipOnly,
            },
        )?;
        let tree = super::git::tree_of(&repository, &locked.commit)?;
        ensure_tree_is_locked(&tree, locked)?;
        super::git::export_tree(&repository, &tree, temp_target)?;
    } else {
        let repository = super::git::clone_without_checkout(&locked.url, temp_target)?;
        super::git::fetch(
            &repository,
            Fetch {
                url: &locked.url,
                reference: &locked.commit,
                history: History::Full,
            },
        )?;
        super::git::checkout_detached(&repository, &locked.commit)?;
        verify_checked_out_tree(temp_target, locked)?;
    }
    ensure_dependency_does_not_reserve_marker(temp_target, locked)?;
    write_ownership_marker(temp_target, locked)?;
    Ok(temp_guard)
}

/// Subdir path: only the requested `subdir` of the repo lands at `target`,
/// with no `.git` directory.
fn prepare_subdir_dependency(request: PrepareSubdirDependencyRequest<'_>) -> Result<TempPath> {
    let PrepareSubdirDependencyRequest {
        temp_root,
        target,
        locked,
        subdir,
    } = request;
    let clone_target = paths::temporary_target_path(temp_root, target, "clone")?;
    let clone_guard = TempPath::new(clone_target);
    let repository = super::git::init(clone_guard.path())?;
    super::git::fetch_directory(
        &repository,
        Directory {
            url: &locked.url,
            commit: &locked.commit,
            subdir,
        },
    )?;
    let subtree = locked_subtree(&repository, locked, subdir)?;

    let extracted_target = paths::temporary_target_path(temp_root, target, "tmp")?;
    let extracted_guard = TempPath::new(extracted_target);
    super::git::export_tree(&repository, &subtree, extracted_guard.path())?;
    drop(clone_guard);

    ensure_dependency_does_not_reserve_marker(extracted_guard.path(), locked)?;
    write_ownership_marker(extracted_guard.path(), locked)?;
    Ok(extracted_guard)
}

/// The tree at `subdir` of the locked commit, which must be the subtree the
/// lockfile records.
fn locked_subtree(
    repository: &gix::Repository,
    locked: &LockedVcsDependency,
    subdir: &str,
) -> Result<String> {
    let expected_subtree = locked.subtree.as_deref().ok_or_else(|| {
        Error::configuration(format!(
            "VCS dependency '{}': locked entry has subdir but no subtree hash",
            locked.path
        ))
    })?;
    let subtree = super::git::entry_at(repository, &locked.commit, subdir)?
        .filter(|entry| entry.kind == "tree")
        .map(|entry| entry.id)
        .ok_or_else(|| {
            Error::configuration(format!(
                "VCS dependency '{}': subdir '{}' not present at locked commit",
                locked.path, subdir
            ))
        })?;
    if subtree != expected_subtree {
        return Err(Error::configuration(format!(
            "VCS dependency '{}': subdir tree {} does not match locked subtree {}",
            locked.path, subtree, expected_subtree
        )));
    }
    Ok(subtree)
}

/// Overlay path: materialize each immediate child of the subtree as its own
/// Overlay path: materialize each immediate child of the subtree as its own
/// snapshot under `target/<child>`. Returns one prepared temp dir per child;
/// the caller installs them individually and never replaces `target` wholesale.
pub(super) fn prepare_overlay_dependency(
    request: PrepareDependencyRequest<'_>,
) -> Result<Vec<(String, TempPath)>> {
    let PrepareDependencyRequest {
        temp_root,
        target,
        locked,
        previous,
    } = request;
    validate_git_input("url", &locked.url)?;
    validate_git_input("reference", &locked.reference)?;
    validate_git_input("commit", &locked.commit)?;
    let subdir = locked.subdir.as_deref().ok_or_else(|| {
        Error::configuration(format!(
            "VCS dependency '{}': overlay requires a subdir",
            locked.path
        ))
    })?;
    ensure_overlay_parent_target(target)?;
    // Refuse to clobber unmanaged or locally-modified content before touching
    // the filesystem. A previous non-overlay entry owned the whole parent, so a
    // clean parent marker authorizes replacing its children during migration.
    if let Some(parent_previous) = previous_parent_materialization(previous) {
        ensure_replaceable_target(target, Some(parent_previous))?;
    } else {
        for child in locked.children.keys() {
            let child_target = target.join(child);
            let child_previous = previous.and_then(|prev| {
                prev.children
                    .get(child)
                    .map(|tree| child_locked(prev, child, tree))
            });
            ensure_replaceable_target(&child_target, child_previous.as_ref())?;
        }
    }

    fs::create_dir_all(temp_root).map_err(|e| Error::configuration(e.to_string()))?;
    let clone_target = paths::temporary_target_path(temp_root, target, "clone")?;
    let clone_guard = TempPath::new(clone_target);
    let repository = super::git::init(clone_guard.path())?;
    super::git::fetch_directory(
        &repository,
        Directory {
            url: &locked.url,
            commit: &locked.commit,
            subdir,
        },
    )?;
    let subtree = locked_subtree(&repository, locked, subdir)?;

    // Guard against the subtree's children drifting from the lockfile between
    // resolve and prepare.
    let actual_children = list_subtree_children(&repository, &subtree, subdir)?;
    if actual_children != locked.children {
        return Err(Error::configuration(format!(
            "VCS dependency '{}': overlay children at the locked commit differ from cuenv.lock; re-run 'cuenv sync vcs'",
            locked.path
        )));
    }

    let mut prepared = Vec::new();
    for (child, child_tree) in &locked.children {
        let child_target = target.join(child);
        let extracted_target = paths::temporary_target_path(temp_root, &child_target, "tmp")?;
        let extracted_guard = TempPath::new(extracted_target);
        super::git::export_tree(&repository, child_tree, extracted_guard.path())?;
        let child_locked = child_locked(locked, child, child_tree);
        ensure_dependency_does_not_reserve_marker(extracted_guard.path(), &child_locked)?;
        write_ownership_marker(extracted_guard.path(), &child_locked)?;
        prepared.push((child.clone(), extracted_guard));
    }
    drop(clone_guard);
    Ok(prepared)
}

/// Check that every managed child of an overlay dependency is materialized,
/// owned, and unmodified.
pub(super) fn check_overlay_materialized(
    target: &Path,
    locked: &LockedVcsDependency,
) -> Result<()> {
    ensure_overlay_parent_target(target)?;
    for (child, child_tree) in &locked.children {
        let child_target = target.join(child);
        let child_locked = child_locked(locked, child, child_tree);
        check_materialized(&child_target, &child_locked)?;
    }
    Ok(())
}

/// Child names that were managed by `previous` but are no longer in the
/// resolved child set. Pure; used for dry-run reporting.
pub(super) fn overlay_stale_children(
    previous: Option<&LockedVcsDependency>,
    resolved: &LockedVcsDependency,
) -> Vec<String> {
    previous
        .filter(|prev| prev.overlay)
        .map(|prev| {
            prev.children
                .keys()
                .filter(|child| !resolved.children.contains_key(*child))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Remove overlay children dropped upstream. A stale child is only removed when
/// it still carries a valid cuenv marker matching its previous commit/tree; if
/// the marker is gone or the content was edited locally, it is left in place as
/// effectively repo-local content. Returns the names actually removed.
pub(super) fn prune_overlay_children(
    target: &Path,
    previous: Option<&LockedVcsDependency>,
    resolved: &LockedVcsDependency,
) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    let Some(previous) = previous.filter(|prev| prev.overlay) else {
        return Ok(removed);
    };
    ensure_overlay_parent_target(target)?;
    for (child, child_tree) in &previous.children {
        if resolved.children.contains_key(child) {
            continue;
        }
        let child_target = target.join(child);
        if fs::symlink_metadata(&child_target).is_err() {
            continue;
        }
        let child_previous = child_locked(previous, child, child_tree);
        if ensure_replaceable_target(&child_target, Some(&child_previous)).is_ok() {
            fs::remove_dir_all(&child_target).map_err(|e| Error::configuration(e.to_string()))?;
            removed.push(child.clone());
        }
    }
    Ok(removed)
}

pub(super) fn install_prepared_dependency(target: &Path, prepared: &Path) -> Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| Error::configuration("VCS target has no parent"))?;
    fs::create_dir_all(parent).map_err(|e| Error::configuration(e.to_string()))?;
    replace_target_with_prepared_checkout(target, prepared)
}

pub(super) fn install_prepared_overlay_dependency(
    request: InstallOverlayDependencyRequest<'_>,
) -> Result<()> {
    let InstallOverlayDependencyRequest {
        temp_root,
        target,
        previous,
        children,
    } = request;
    ensure_overlay_parent_target(target)?;
    if let Some(parent_previous) = previous_parent_materialization(previous) {
        ensure_replaceable_target(target, Some(parent_previous))?;
        let parent_target = paths::temporary_target_path(temp_root, target, "overlay")?;
        let parent_guard = TempPath::new(parent_target);
        fs::create_dir_all(parent_guard.path()).map_err(|e| Error::configuration(e.to_string()))?;
        for (child, temp) in children {
            fs::rename(temp.path(), parent_guard.path().join(child))
                .map_err(|e| Error::configuration(e.to_string()))?;
        }
        install_prepared_dependency(target, parent_guard.path())?;
        return Ok(());
    }

    for (child, temp) in children {
        install_prepared_dependency(&target.join(child), temp.path())?;
    }
    Ok(())
}

fn validate_git_input(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.starts_with('-') || value.chars().any(|c| c.is_control()) {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency {label} '{}'",
            value
        )));
    }
    Ok(())
}

fn previous_parent_materialization(
    previous: Option<&LockedVcsDependency>,
) -> Option<&LockedVcsDependency> {
    previous.filter(|previous| !previous.overlay)
}

fn ensure_overlay_parent_target(target: &Path) -> Result<()> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(Error::configuration(format!(
                "Refusing to use symlinked VCS overlay parent '{}'",
                target.display()
            )));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(Error::configuration(format!(
                "Refusing to use non-directory VCS overlay parent '{}'",
                target.display()
            )));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::configuration(e.to_string())),
    }
    Ok(())
}

fn ensure_replaceable_target(target: &Path, previous: Option<&LockedVcsDependency>) -> Result<()> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(Error::configuration(format!(
                "Refusing to overwrite symlinked VCS target '{}'",
                target.display()
            )));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::configuration(e.to_string())),
    }
    let marker = read_ownership_marker(target).map_err(|_| {
        Error::configuration(format!(
            "Refusing to overwrite unmanaged VCS target '{}'",
            target.display()
        ))
    })?;
    if marker.trim().is_empty() {
        return Err(Error::configuration(format!(
            "Refusing to overwrite VCS target '{}' with an invalid ownership marker",
            target.display()
        )));
    }
    let previous = previous.ok_or_else(|| {
        Error::configuration(format!(
            "Refusing to overwrite VCS target '{}' without an existing lock entry",
            target.display()
        ))
    })?;
    if marker.trim() != previous.commit {
        return Err(Error::configuration(format!(
            "Refusing to overwrite VCS target '{}' with ownership marker {}, expected {}",
            target.display(),
            marker.trim(),
            previous.commit
        )));
    }
    validate_git_input("marker", marker.trim())?;
    if is_snapshot_materialization(previous) {
        let expected = expected_snapshot_tree(previous);
        let actual = vendored_tree_hash(target)?;
        if actual != expected {
            return Err(Error::configuration(format!(
                "Refusing to overwrite modified VCS target '{}': tree {}, expected {}",
                target.display(),
                actual,
                expected
            )));
        }
    } else {
        ensure_git_dir(target)?;
        ensure_marker_is_excluded(target)?;
        if !only_marker_changed(&super::git::changes(&super::git::open(target)?)?) {
            return Err(Error::configuration(format!(
                "Refusing to overwrite dirty VCS checkout '{}'",
                target.display()
            )));
        }
    }
    Ok(())
}

fn ensure_dependency_does_not_reserve_marker(
    path: &Path,
    locked: &LockedVcsDependency,
) -> Result<()> {
    if fs::symlink_metadata(path.join(MARKER_FILE)).is_ok() {
        return Err(Error::configuration(format!(
            "VCS dependency '{}' contains reserved cuenv ownership marker '{}'",
            locked.path, MARKER_FILE
        )));
    }
    Ok(())
}

fn write_ownership_marker(path: &Path, locked: &LockedVcsDependency) -> Result<()> {
    let mut marker = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path.join(MARKER_FILE))
        .map_err(|e| Error::configuration(e.to_string()))?;
    marker
        .write_all(locked.commit.as_bytes())
        .map_err(|e| Error::configuration(e.to_string()))?;
    if is_git_checkout_materialization(locked) {
        ensure_marker_is_excluded(path)?;
    }
    Ok(())
}

fn read_ownership_marker(path: &Path) -> Result<String> {
    let marker_path = path.join(MARKER_FILE);
    let metadata = fs::symlink_metadata(&marker_path).map_err(|e| {
        Error::configuration(format!(
            "Unable to read VCS ownership marker '{}': {}",
            marker_path.display(),
            e
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Error::configuration(format!(
            "Refusing to read symlinked or non-file VCS ownership marker '{}'",
            marker_path.display()
        )));
    }
    fs::read_to_string(&marker_path).map_err(|e| {
        Error::configuration(format!(
            "Unable to read VCS ownership marker '{}': {}",
            marker_path.display(),
            e
        ))
    })
}

fn ensure_git_dir(path: &Path) -> Result<()> {
    let git_dir = path.join(".git");
    let metadata = fs::symlink_metadata(&git_dir).map_err(|e| {
        Error::configuration(format!(
            "Refusing to use malformed VCS checkout '{}' without a git directory: {}",
            path.display(),
            e
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::configuration(format!(
            "Refusing to use VCS checkout '{}' with a symlinked or non-directory .git path",
            path.display()
        )));
    }
    Ok(())
}

fn ensure_marker_is_excluded(path: &Path) -> Result<()> {
    ensure_git_dir(path)?;
    let info_dir = path.join(".git/info");
    let metadata = fs::symlink_metadata(&info_dir).map_err(|e| {
        Error::configuration(format!(
            "Refusing to update malformed VCS checkout '{}' without .git/info: {}",
            path.display(),
            e
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::configuration(format!(
            "Refusing to update VCS checkout '{}' with a symlinked or non-directory .git/info path",
            path.display()
        )));
    }
    let exclude_path = path.join(".git/info/exclude");
    if fs::symlink_metadata(&exclude_path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(Error::configuration(format!(
            "Refusing to update symlinked VCS exclude file '{}'",
            exclude_path.display()
        )));
    }
    let existing = fs::read_to_string(&exclude_path).unwrap_or_default();
    if existing.lines().any(|line| line.trim() == MARKER_FILE) {
        return Ok(());
    }
    let mut next = existing;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(MARKER_FILE);
    next.push('\n');
    fs::write(exclude_path, next).map_err(|e| Error::configuration(e.to_string()))?;
    Ok(())
}

/// Whether nothing in a checkout differs from `HEAD` but cuenv's own marker.
fn only_marker_changed(changes: &[String]) -> bool {
    changes.iter().all(|path| path == MARKER_FILE)
}

fn replace_target_with_prepared_checkout(target: &Path, prepared: &Path) -> Result<()> {
    match fs::symlink_metadata(target) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::rename(prepared, target).map_err(|e| Error::configuration(e.to_string()))?;
            return Ok(());
        }
        Err(e) => return Err(Error::configuration(e.to_string())),
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(Error::configuration(format!(
                "Refusing to replace symlinked VCS target '{}'",
                target.display()
            )));
        }
        Ok(_) => {}
    }

    let backup = paths::temporary_backup_path(target)?;
    fs::rename(target, &backup).map_err(|e| Error::configuration(e.to_string()))?;
    if let Err(rename_error) = fs::rename(prepared, target) {
        let restore_result = fs::rename(&backup, target);
        return Err(Error::configuration(format!(
            "Failed to replace VCS target '{}': {}; restore {}",
            target.display(),
            rename_error,
            if restore_result.is_ok() {
                "succeeded"
            } else {
                "failed"
            }
        )));
    }
    fs::remove_dir_all(&backup).map_err(|e| Error::configuration(e.to_string()))?;
    Ok(())
}

fn verify_checked_out_tree(path: &Path, locked: &LockedVcsDependency) -> Result<()> {
    let repository = super::git::open(path)?;
    let head = super::git::head_commit(&repository)?;
    ensure_tree_is_locked(&super::git::tree_of(&repository, &head)?, locked)
}

fn ensure_tree_is_locked(tree: &str, locked: &LockedVcsDependency) -> Result<()> {
    if tree != locked.tree {
        return Err(Error::configuration(format!(
            "VCS dependency '{}' resolved tree {}, expected {}",
            locked.path, tree, locked.tree
        )));
    }
    Ok(())
}

fn is_snapshot_materialization(locked: &LockedVcsDependency) -> bool {
    locked.vendor || locked.subdir.is_some()
}

fn is_git_checkout_materialization(locked: &LockedVcsDependency) -> bool {
    !locked.vendor && locked.subdir.is_none()
}

/// Tree object the snapshot content on disk should hash to. When a `subdir`
/// is set we expect the subtree, not the full repo root tree.
fn expected_snapshot_tree(locked: &LockedVcsDependency) -> &str {
    locked.subtree.as_deref().unwrap_or(locked.tree.as_str())
}

pub(super) fn check_materialized(path: &Path, locked: &LockedVcsDependency) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(Error::configuration(format!(
                "VCS dependency '{}' is a symlink",
                locked.path
            )));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::configuration(format!(
                "VCS dependency '{}' is missing",
                locked.path
            )));
        }
        Err(e) => return Err(Error::configuration(e.to_string())),
    }
    let marker = read_ownership_marker(path).map_err(|_| {
        Error::configuration(format!(
            "VCS dependency '{}' is missing cuenv ownership marker",
            locked.path
        ))
    })?;
    if marker.trim() != locked.commit {
        return Err(Error::configuration(format!(
            "VCS dependency '{}' marker is {}, expected {}",
            locked.path,
            marker.trim(),
            locked.commit
        )));
    }
    if is_snapshot_materialization(locked) {
        let expected = expected_snapshot_tree(locked);
        let actual = vendored_tree_hash(path)?;
        if actual != expected {
            return Err(Error::configuration(format!(
                "VCS dependency '{}' has tree {}, expected {}",
                locked.path, actual, expected
            )));
        }
        return Ok(());
    }
    let head = super::git::head_commit(&super::git::open(path)?)?;
    if head != locked.commit {
        return Err(Error::configuration(format!(
            "VCS dependency '{}' is checked out at {}, expected {}",
            locked.path, head, locked.commit
        )));
    }
    verify_checked_out_tree(path, locked)?;
    if !only_marker_changed(&super::git::changes(&super::git::open(path)?)?) {
        return Err(Error::configuration(format!(
            "VCS dependency '{}' has uncommitted changes",
            locked.path
        )));
    }
    Ok(())
}

fn vendored_tree_hash(path: &Path) -> Result<String> {
    super::git::directory_tree(path, MARKER_FILE)
}
