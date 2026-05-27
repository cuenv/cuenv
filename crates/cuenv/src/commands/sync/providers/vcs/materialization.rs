//! VCS dependency checkout and materialization helpers.

use super::git::{git_output, run_git};
use super::paths::{
    TempPath, ensure_managed_internal_path, temporary_backup_path, temporary_target_path,
    validate_materialization_path, validate_subdir,
};
use super::{
    FETCH_HEAD_COMMIT, FETCH_HEAD_TREE, HEAD_TREE, MARKER_FILE, PrepareDependencyRequest,
    PrepareSubdirDependencyRequest,
};
use cuenv_core::lockfile::LockedVcsDependency;
use cuenv_core::manifest::VcsDependency;
use cuenv_core::{Error, Result};
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::Path;

pub(super) fn prune_removed_vcs_dependencies(
    git_root: &Path,
    dependencies: &[LockedVcsDependency],
) -> Result<()> {
    for dependency in dependencies {
        let target = validate_materialization_path(git_root, &dependency.path)?;
        if fs::symlink_metadata(&target).is_ok() {
            ensure_replaceable_target(&target, Some(dependency))?;
            fs::remove_dir_all(&target).map_err(|e| Error::configuration(e.to_string()))?;
        }
    }
    Ok(())
}

pub(super) fn should_update(name: &str, update: Option<&Vec<String>>) -> bool {
    match update {
        None => false,
        Some(names) if names.is_empty() => true,
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
    })
}

pub(super) fn resolve_dependency(
    cache_root: &Path,
    name: &str,
    spec: &VcsDependency,
) -> Result<LockedVcsDependency> {
    validate_git_input("url", &spec.url)?;
    validate_git_input("reference", &spec.reference)?;
    let normalized_subdir = spec.subdir.as_deref().map(validate_subdir).transpose()?;
    let cache_path = cache_root.join(name);
    fs::create_dir_all(cache_root).map_err(|e| Error::configuration(e.to_string()))?;
    ensure_managed_internal_path(cache_root, &cache_path)?;
    if cache_path.exists() {
        ensure_git_dir(&cache_path)?;
        run_git(
            [
                OsStr::new("remote"),
                OsStr::new("set-url"),
                OsStr::new("origin"),
                OsStr::new(&spec.url),
            ],
            Some(&cache_path),
        )?;
    } else {
        run_git(
            [
                OsStr::new("clone"),
                OsStr::new("--no-checkout"),
                OsStr::new(&spec.url),
                cache_path.as_os_str(),
            ],
            None,
        )?;
    }
    run_git(
        [
            OsStr::new("fetch"),
            OsStr::new("--tags"),
            OsStr::new("origin"),
            OsStr::new(&spec.reference),
        ],
        Some(&cache_path),
    )?;
    let commit = git_output(
        [OsStr::new("rev-parse"), OsStr::new(FETCH_HEAD_COMMIT)],
        Some(&cache_path),
    )?;
    let tree = git_output(
        [OsStr::new("rev-parse"), OsStr::new(FETCH_HEAD_TREE)],
        Some(&cache_path),
    )?;

    let subtree = if let Some(ref subdir) = normalized_subdir {
        let object_ref = format!("{}:{}", commit, subdir);
        let object_type = git_output(
            [
                OsStr::new("cat-file"),
                OsStr::new("-t"),
                OsStr::new(&object_ref),
            ],
            Some(&cache_path),
        )
        .map_err(|_| {
            Error::configuration(format!(
                "VCS dependency '{}': subdir '{}' not found at reference '{}'",
                name, subdir, spec.reference
            ))
        })?;
        if object_type != "tree" {
            return Err(Error::configuration(format!(
                "VCS dependency '{}': subdir '{}' must be a tree (directory), got {} at reference '{}'",
                name, subdir, object_type, spec.reference
            )));
        }
        Some(git_output(
            [OsStr::new("rev-parse"), OsStr::new(&object_ref)],
            Some(&cache_path),
        )?)
    } else {
        None
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
    })
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
    let temp_target = temporary_target_path(temp_root, target, "tmp")?;
    let temp_guard = TempPath::new(temp_target);
    let temp_target = temp_guard.path();
    run_git(
        [
            OsStr::new("clone"),
            OsStr::new("--no-checkout"),
            OsStr::new(&locked.url),
            temp_target.as_os_str(),
        ],
        None,
    )?;
    run_git(
        [
            OsStr::new("fetch"),
            OsStr::new("origin"),
            OsStr::new(&locked.commit),
        ],
        Some(temp_target),
    )?;
    run_git(
        [
            OsStr::new("checkout"),
            OsStr::new("--detach"),
            OsStr::new(&locked.commit),
        ],
        Some(temp_target),
    )?;
    verify_checked_out_tree(temp_target, locked)?;
    ensure_dependency_does_not_reserve_marker(temp_target, locked)?;
    if locked.vendor {
        let git_dir = temp_target.join(".git");
        if git_dir.exists() {
            fs::remove_dir_all(&git_dir).map_err(|e| Error::configuration(e.to_string()))?;
        }
    }
    write_ownership_marker(temp_target, locked)?;
    Ok(temp_guard)
}

/// Sparse-checkout subdir path: only the requested `subdir` of the repo lands
/// at `target`, with no `.git` directory.
fn prepare_subdir_dependency(request: PrepareSubdirDependencyRequest<'_>) -> Result<TempPath> {
    let PrepareSubdirDependencyRequest {
        temp_root,
        target,
        locked,
        subdir,
    } = request;
    let expected_subtree = locked.subtree.as_deref().ok_or_else(|| {
        Error::configuration(format!(
            "VCS dependency '{}': locked entry has subdir but no subtree hash",
            locked.path
        ))
    })?;
    let clone_target = temporary_target_path(temp_root, target, "clone")?;
    let clone_guard = TempPath::new(clone_target);
    let clone_path = clone_guard.path();
    run_git(
        [
            OsStr::new("clone"),
            OsStr::new("--no-checkout"),
            OsStr::new("--filter=blob:none"),
            OsStr::new(&locked.url),
            clone_path.as_os_str(),
        ],
        None,
    )?;
    run_git(
        [
            OsStr::new("sparse-checkout"),
            OsStr::new("init"),
            OsStr::new("--cone"),
        ],
        Some(clone_path),
    )?;
    run_git(
        [
            OsStr::new("sparse-checkout"),
            OsStr::new("set"),
            OsStr::new("--"),
            OsStr::new(subdir),
        ],
        Some(clone_path),
    )?;
    run_git(
        [
            OsStr::new("fetch"),
            OsStr::new("origin"),
            OsStr::new(&locked.commit),
        ],
        Some(clone_path),
    )?;
    run_git(
        [
            OsStr::new("checkout"),
            OsStr::new("--detach"),
            OsStr::new(&locked.commit),
        ],
        Some(clone_path),
    )?;
    let subtree = git_output(
        [
            OsStr::new("rev-parse"),
            OsStr::new(&format!("HEAD:{}", subdir)),
        ],
        Some(clone_path),
    )
    .map_err(|_| {
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

    let extracted_source = clone_path.join(subdir);
    if !extracted_source.is_dir() {
        return Err(Error::configuration(format!(
            "VCS dependency '{}': sparse checkout did not materialize subdir '{}'",
            locked.path, subdir
        )));
    }
    let extracted_target = temporary_target_path(temp_root, target, "tmp")?;
    let extracted_guard = TempPath::new(extracted_target);
    fs::rename(&extracted_source, extracted_guard.path())
        .map_err(|e| Error::configuration(e.to_string()))?;
    drop(clone_guard);

    ensure_dependency_does_not_reserve_marker(extracted_guard.path(), locked)?;
    write_ownership_marker(extracted_guard.path(), locked)?;
    Ok(extracted_guard)
}

pub(super) fn install_prepared_dependency(target: &Path, prepared: &Path) -> Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| Error::configuration("VCS target has no parent"))?;
    fs::create_dir_all(parent).map_err(|e| Error::configuration(e.to_string()))?;
    replace_target_with_prepared_checkout(target, prepared)
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
        let status = git_output(
            [
                OsStr::new("status"),
                OsStr::new("--porcelain"),
                OsStr::new("--ignored"),
            ],
            Some(target),
        )?;
        if !git_status_is_clean_or_marker_only(&status) {
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

fn git_status_is_clean_or_marker_only(status: &str) -> bool {
    status.lines().all(|line| {
        line.trim().is_empty()
            || line == format!("?? {MARKER_FILE}")
            || line == format!("!! {MARKER_FILE}")
    })
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

    let backup = temporary_backup_path(target)?;
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
    let tree = git_output([OsStr::new("rev-parse"), OsStr::new(HEAD_TREE)], Some(path))?;
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
    let head = git_output([OsStr::new("rev-parse"), OsStr::new("HEAD")], Some(path))?;
    if head != locked.commit {
        return Err(Error::configuration(format!(
            "VCS dependency '{}' is checked out at {}, expected {}",
            locked.path, head, locked.commit
        )));
    }
    verify_checked_out_tree(path, locked)?;
    let status = git_output(
        [
            OsStr::new("status"),
            OsStr::new("--porcelain"),
            OsStr::new("--ignored"),
        ],
        Some(path),
    )?;
    if !git_status_is_clean_or_marker_only(&status) {
        return Err(Error::configuration(format!(
            "VCS dependency '{}' has uncommitted changes",
            locked.path
        )));
    }
    Ok(())
}

fn vendored_tree_hash(path: &Path) -> Result<String> {
    let temp = std::env::temp_dir().join(format!(
        "cuenv-vcs-tree-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| Error::configuration(e.to_string()))?
            .as_nanos()
    ));
    fs::create_dir_all(&temp).map_err(|e| Error::configuration(e.to_string()))?;
    let result = vendored_tree_hash_with_git_dir(path, &temp);
    let cleanup = fs::remove_dir_all(&temp);
    match (result, cleanup) {
        (Ok(tree), Ok(())) => Ok(tree),
        (Ok(_), Err(e)) => Err(Error::configuration(e.to_string())),
        (Err(e), _) => Err(e),
    }
}

fn vendored_tree_hash_with_git_dir(path: &Path, temp: &Path) -> Result<String> {
    run_git([OsStr::new("init"), OsStr::new("-q")], Some(temp))?;
    let git_dir = temp.join(".git");
    run_git(
        [
            OsStr::new("--git-dir"),
            git_dir.as_os_str(),
            OsStr::new("--work-tree"),
            path.as_os_str(),
            OsStr::new("add"),
            OsStr::new("--all"),
            OsStr::new("--force"),
            OsStr::new("--"),
            OsStr::new("."),
        ],
        None,
    )?;
    run_git(
        [
            OsStr::new("--git-dir"),
            git_dir.as_os_str(),
            OsStr::new("--work-tree"),
            path.as_os_str(),
            OsStr::new("rm"),
            OsStr::new("--cached"),
            OsStr::new("--ignore-unmatch"),
            OsStr::new("--"),
            OsStr::new(MARKER_FILE),
        ],
        None,
    )?;
    git_output(
        [
            OsStr::new("--git-dir"),
            git_dir.as_os_str(),
            OsStr::new("--work-tree"),
            path.as_os_str(),
            OsStr::new("write-tree"),
        ],
        None,
    )
}
