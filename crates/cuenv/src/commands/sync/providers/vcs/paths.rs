//! VCS dependency path validation and temporary path guards.

use cuenv_core::{Error, Result};
use std::fs;
use std::path::{Component, Path, PathBuf};

pub(super) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.starts_with('.')
    {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency name '{}'",
            name
        )));
    }
    Ok(())
}

pub(super) fn temporary_cache_root() -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "cuenv-vcs-cache-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| Error::configuration(e.to_string()))?
            .as_nanos()
    ));
    Ok(path)
}

pub(super) struct TempPath {
    path: PathBuf,
}

impl TempPath {
    pub(super) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub(super) fn validate_materialization_path(git_root: &Path, path: &str) -> Result<PathBuf> {
    let rel = Path::new(path);
    if rel.is_absolute() || path.trim().is_empty() {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency path '{}': path must be relative and contained in the repository",
            path
        )));
    }
    let mut components = Vec::new();
    for component in rel.components() {
        let Component::Normal(value) = component else {
            return Err(Error::configuration(format!(
                "Invalid VCS dependency path '{}': path must not contain '.', '..', or prefixes",
                path
            )));
        };
        let value = value.to_string_lossy();
        validate_path_component(&value, path)?;
        components.push(value.into_owned());
    }
    if components.is_empty() {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency path '{}': path must not target the repository root",
            path
        )));
    }
    if components.iter().any(|component| component == ".git")
        || components_start_with(&components, &[".cuenv", "vcs", "cache"])
        || components_start_with(&components, &[".cuenv", "vcs", "tmp"])
    {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency path '{}': path targets cuenv or git internals",
            path
        )));
    }
    let target = components
        .iter()
        .fold(git_root.to_path_buf(), |acc, part| acc.join(part));
    ensure_parent_stays_in_repo(git_root, &target)?;
    Ok(target)
}

fn components_start_with(components: &[String], prefix: &[&str]) -> bool {
    components.len() >= prefix.len()
        && components
            .iter()
            .zip(prefix)
            .all(|(component, expected)| component == expected)
}

pub(super) fn validate_overlay_child_name(name: &str, context: &str) -> Result<()> {
    validate_path_component(name, context).map_err(|_| {
        Error::configuration(format!(
            "Invalid VCS dependency overlay child '{name}' in subtree '{context}': child names may only use literal safe names"
        ))
    })?;
    if name == ".git" {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency overlay child '{name}' in subtree '{context}': child must not be '.git'"
        )));
    }
    Ok(())
}

fn validate_path_component(component: &str, original_path: &str) -> Result<()> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.starts_with('-')
        || component.contains('\\')
        || component.chars().any(|c| {
            c.is_control()
                || matches!(
                    c,
                    '*' | '?' | '[' | ']' | '!' | '#' | ' ' | '\t' | '\n' | '\r'
                )
        })
    {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency path '{}': path components may only use literal safe names",
            original_path
        )));
    }
    Ok(())
}

/// Validate a sparse-checkout `subdir`. Returns the canonical slash-joined
/// relative path to use with `git sparse-checkout set` and `git cat-file/rev-parse`.
///
/// The input is required to be in canonical form: forward-slash separated, no
/// leading/trailing whitespace, no leading/trailing slashes, no empty
/// components. Non-canonical inputs are rejected so the value can be compared
/// to the lockfile entry by string equality without re-canonicalizing.
pub(super) fn validate_subdir(subdir: &str) -> Result<String> {
    if subdir.is_empty() {
        return Err(Error::configuration(
            "Invalid VCS dependency subdir: must not be empty",
        ));
    }
    if subdir.trim() != subdir {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency subdir '{}': must not have leading or trailing whitespace",
            subdir
        )));
    }
    if subdir.contains('\\') {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency subdir '{}': must use forward-slash separators",
            subdir
        )));
    }
    if subdir.starts_with('/') || subdir.ends_with('/') {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency subdir '{}': must not start or end with '/'",
            subdir
        )));
    }
    let components: Vec<&str> = subdir.split('/').collect();
    for component in &components {
        if component.is_empty() {
            return Err(Error::configuration(format!(
                "Invalid VCS dependency subdir '{}': must not contain empty components",
                subdir
            )));
        }
        if *component == "." || *component == ".." {
            return Err(Error::configuration(format!(
                "Invalid VCS dependency subdir '{}': must not contain '.' or '..'",
                subdir
            )));
        }
        validate_path_component(component, subdir)?;
    }
    Ok(components.join("/"))
}

fn ensure_parent_stays_in_repo(git_root: &Path, target: &Path) -> Result<()> {
    let canonical_root = git_root
        .canonicalize()
        .map_err(|e| Error::configuration(format!("Failed to canonicalize git root: {e}")))?;
    let mut current = canonical_root.clone();
    let relative = target.strip_prefix(git_root).map_err(|_| {
        Error::configuration(format!(
            "Invalid VCS dependency path '{}': path must stay within the repository",
            target.display()
        ))
    })?;
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(component) = component else {
            return Err(Error::configuration(format!(
                "Invalid VCS dependency path '{}': path must stay within the repository",
                target.display()
            )));
        };
        if components.peek().is_none() {
            break;
        }
        current.push(component);
        if fs::symlink_metadata(&current).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(Error::configuration(format!(
                "Invalid VCS dependency path '{}': parent path contains a symlink",
                target.display()
            )));
        }
        if current.exists() {
            let canonical = current.canonicalize().map_err(|e| {
                Error::configuration(format!(
                    "Failed to canonicalize VCS dependency parent '{}': {}",
                    current.display(),
                    e
                ))
            })?;
            if !canonical.starts_with(&canonical_root) {
                return Err(Error::configuration(format!(
                    "Invalid VCS dependency path '{}': parent resolves outside the repository",
                    target.display()
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn ensure_managed_internal_path(git_root: &Path, target: &Path) -> Result<()> {
    ensure_parent_stays_in_repo(git_root, target)?;
    if fs::symlink_metadata(target).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(Error::configuration(format!(
            "Refusing to write symlinked cuenv-managed path '{}'",
            target.display()
        )));
    }
    if target.exists() {
        let canonical_root = git_root
            .canonicalize()
            .map_err(|e| Error::configuration(format!("Failed to canonicalize git root: {e}")))?;
        let canonical_target = target.canonicalize().map_err(|e| {
            Error::configuration(format!(
                "Failed to canonicalize cuenv-managed path '{}': {}",
                target.display(),
                e
            ))
        })?;
        if !canonical_target.starts_with(canonical_root) {
            return Err(Error::configuration(format!(
                "Refusing to write cuenv-managed path '{}' outside the repository",
                target.display()
            )));
        }
    }
    Ok(())
}

pub(super) fn temporary_target_path(
    temp_root: &Path,
    target: &Path,
    kind: &str,
) -> Result<PathBuf> {
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dependency");
    let path = temp_root.join(format!(
        ".{file_name}.cuenv-{kind}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| Error::configuration(e.to_string()))?
            .as_nanos()
    ));
    if fs::symlink_metadata(&path).is_ok() {
        return Err(Error::configuration(format!(
            "Refusing to reuse existing VCS temporary path '{}'",
            path.display()
        )));
    }
    Ok(path)
}

pub(super) fn temporary_backup_path(target: &Path) -> Result<PathBuf> {
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dependency");
    let path = target.with_file_name(format!(
        ".{file_name}.cuenv-backup-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| Error::configuration(e.to_string()))?
            .as_nanos()
    ));
    if fs::symlink_metadata(&path).is_ok() {
        return Err(Error::configuration(format!(
            "Refusing to reuse existing VCS backup path '{}'",
            path.display()
        )));
    }
    Ok(path)
}
