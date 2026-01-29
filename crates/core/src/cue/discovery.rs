//! env.cue file detection and package validation.
//!
//! Provides utilities for finding and validating env.cue files
//! within a CUE module hierarchy.

use crate::{Error, Result};
use ignore::WalkBuilder;
use std::fmt::Display;
use std::fs;
use std::path::{Path, PathBuf};

/// Compute relative path from module root to target directory.
///
/// Returns `"."` if the paths are equal or if stripping fails.
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use cuenv_core::cue::discovery::compute_relative_path;
///
/// let module_root = Path::new("/repo");
/// let target = Path::new("/repo/services/api");
/// assert_eq!(compute_relative_path(target, module_root), "services/api");
///
/// // Same path returns "."
/// assert_eq!(compute_relative_path(module_root, module_root), ".");
/// ```
#[must_use]
pub fn compute_relative_path(target: &Path, module_root: &Path) -> String {
    target.strip_prefix(module_root).map_or_else(
        |_| ".".to_string(),
        |p| {
            if p.as_os_str().is_empty() {
                ".".to_string()
            } else {
                p.to_string_lossy().to_string()
            }
        },
    )
}

/// Adjust a meta key path by replacing `"./"` prefix with the relative path.
///
/// Meta keys are formatted as `"instance_path/field_path"`. When an instance
/// path starts with `"./"`, this function replaces it with the actual relative
/// path from the module root.
///
/// # Examples
///
/// ```
/// use cuenv_core::cue::discovery::adjust_meta_key_path;
///
/// assert_eq!(adjust_meta_key_path("./tasks/build", "services/api"), "services/api/tasks/build");
/// assert_eq!(adjust_meta_key_path("other/path", "services/api"), "other/path");
/// ```
#[must_use]
pub fn adjust_meta_key_path(meta_key: &str, relative_path: &str) -> String {
    if meta_key.starts_with("./") {
        meta_key.replacen("./", &format!("{relative_path}/"), 1)
    } else {
        meta_key.to_string()
    }
}

/// Format a list of evaluation errors into a human-readable summary.
///
/// Each error is formatted as `"  path: error_message"` on its own line.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
/// use cuenv_core::cue::discovery::format_eval_errors;
///
/// let errors = vec![
///     (PathBuf::from("/repo/a"), "syntax error"),
///     (PathBuf::from("/repo/b"), "missing field"),
/// ];
/// let summary = format_eval_errors(&errors);
/// assert!(summary.contains("/repo/a: syntax error"));
/// assert!(summary.contains("/repo/b: missing field"));
/// ```
#[must_use]
pub fn format_eval_errors<E: Display>(errors: &[(PathBuf, E)]) -> String {
    errors
        .iter()
        .map(|(dir, e)| format!("  {}: {e}", dir.display()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Status of env.cue file detection.
#[derive(Debug)]
#[allow(missing_docs)]
pub enum EnvFileStatus {
    /// No env.cue present in the directory
    Missing,
    /// env.cue exists but does not match the expected package
    PackageMismatch { found_package: Option<String> },
    /// env.cue exists and matches the expected package. Contains canonical directory path.
    Match(PathBuf),
}

/// Locate env.cue in `path` and ensure it declares the expected package.
///
/// # Errors
///
/// Returns an error if the env.cue file cannot be read or path canonicalization fails.
pub fn find_env_file(path: &Path, expected_package: &str) -> Result<EnvFileStatus> {
    let directory = normalize_path(path)?;

    let env_file = directory.join("env.cue");
    if !env_file.exists() {
        return Ok(EnvFileStatus::Missing);
    }

    let package_name = detect_package_name(&env_file)?;
    if package_name.as_deref() != Some(expected_package) {
        return Ok(EnvFileStatus::PackageMismatch {
            found_package: package_name,
        });
    }

    let canonical = directory
        .canonicalize()
        .map_err(|e| Error::configuration(format!("Failed to canonicalize path: {e}")))?;

    Ok(EnvFileStatus::Match(canonical))
}

/// Detect the CUE package name from a file.
///
/// Returns `Ok(None)` if no package declaration is found.
pub fn detect_package_name(path: &Path) -> Result<Option<String>> {
    let contents = fs::read_to_string(path)
        .map_err(|e| Error::configuration(format!("Failed to read {}: {e}", path.display())))?;

    let cleaned = strip_comments(contents.trim_start_matches('\u{feff}'));

    for line in cleaned.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("package ") {
            if let Some(name) = rest.split_whitespace().next()
                && !name.is_empty()
            {
                return Ok(Some(name.to_string()));
            }
            return Ok(None);
        }
        break;
    }

    Ok(None)
}

/// Find the CUE module root by walking up from `start`.
///
/// Looks for a `cue.mod/` directory. Returns `None` if no cue.mod is found
/// (will walk to filesystem root).
#[must_use]
pub fn find_cue_module_root(start: &Path) -> Option<PathBuf> {
    let mut current = normalize_path(start).ok()?;

    // Canonicalize to resolve symlinks
    current = current.canonicalize().ok()?;

    loop {
        if current.join("cue.mod").is_dir() {
            return Some(current);
        }

        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => return None,
        }
    }
}

/// Walk up from `start` collecting directories containing env.cue files.
///
/// Stops at the CUE module root (directory containing `cue.mod/`) or filesystem root.
/// Returns directories in order from root to leaf (ancestor first).
///
/// # Errors
///
/// Returns an error if the current directory cannot be obtained or paths cannot be resolved.
pub fn find_ancestor_env_files(start: &Path, expected_package: &str) -> Result<Vec<PathBuf>> {
    let start_canonical = resolve_start_path(start)?;
    let module_root = find_cue_module_root(&start_canonical);

    let ancestors =
        collect_ancestor_env_files(start_canonical, module_root.as_deref(), expected_package)?;
    Ok(ancestors)
}

fn collect_ancestor_env_files(
    start: PathBuf,
    module_root: Option<&Path>,
    expected_package: &str,
) -> Result<Vec<PathBuf>> {
    let mut ancestors = Vec::new();
    let mut current = start;

    loop {
        if let EnvFileStatus::Match(dir) = find_env_file(&current, expected_package)? {
            ancestors.push(dir);
        }

        if module_root.is_some_and(|root| current == root) {
            break;
        }

        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
    }

    ancestors.reverse();
    Ok(ancestors)
}

/// Discover all directories containing env.cue files with matching package.
///
/// Uses the `ignore` crate to walk the filesystem while respecting `.gitignore`.
/// Returns directories (not file paths) that contain env.cue files with the
/// expected package declaration.
///
/// # Arguments
/// * `module_root` - The CUE module root directory (must contain cue.mod/)
/// * `expected_package` - The CUE package name to filter for
///
/// # Returns
/// A vector of directory paths (relative to module_root) containing matching env.cue files.
/// The paths are suitable for use with `cuengine::evaluate_module` with `TargetDir` option.
#[must_use]
pub fn discover_env_cue_directories(module_root: &Path, expected_package: &str) -> Vec<PathBuf> {
    let mut directories = Vec::new();

    let walker = WalkBuilder::new(module_root)
        .follow_links(false)
        .standard_filters(true)
        .build();

    for result in walker {
        let Ok(entry) = result else {
            continue;
        };

        let path = entry.path();
        if !is_env_cue_file(path) {
            continue;
        }

        if !matches_package(path, expected_package) {
            continue;
        }

        if let Some(dir) = path.parent()
            && let Ok(canonical) = dir.canonicalize()
        {
            directories.push(canonical);
        }
    }

    directories
}

fn matches_package(path: &Path, expected_package: &str) -> bool {
    let Ok(package_name) = detect_package_name(path) else {
        return false;
    };

    package_name.as_deref() == Some(expected_package)
}

fn is_env_cue_file(path: &Path) -> bool {
    path.file_name() == Some("env.cue".as_ref())
}

fn resolve_start_path(start: &Path) -> Result<PathBuf> {
    normalize_path(start)?
        .canonicalize()
        .map_err(|e| Error::configuration(format!("Failed to canonicalize path: {e}")))
}

fn normalize_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map_err(|e| Error::configuration(format!("Failed to get current directory: {e}")))
            .map(|cwd| cwd.join(path))
    }
}

fn strip_comments(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '/' {
            match chars.peek() {
                Some('/') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if next == '\n' {
                            result.push('\n');
                            break;
                        }
                    }
                    continue;
                }
                Some('*') => {
                    chars.next();
                    let mut prev = '\0';
                    for next in chars.by_ref() {
                        if prev == '*' && next == '/' {
                            break;
                        }
                        prev = next;
                    }
                    continue;
                }
                _ => {}
            }
        }

        result.push(ch);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::{
        EnvFileStatus, adjust_meta_key_path, compute_relative_path, detect_package_name,
        find_ancestor_env_files, find_cue_module_root, find_env_file, format_eval_errors,
        strip_comments,
    };
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use tempfile::{NamedTempFile, TempDir};

    #[test]
    fn strip_comments_removes_line_and_block_comments() {
        let source = r#"
// line comment
/* block
comment */
package cuenv // inline
        "#;
        let cleaned = strip_comments(source);
        assert!(cleaned.contains("package cuenv"));
        assert!(!cleaned.contains("line comment"));
        assert!(!cleaned.contains("block"));
    }

    #[test]
    fn detect_package_name_finds_package() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "// comment\npackage cuenv // inline\n\nenv: {{}}").unwrap();

        let package = detect_package_name(Path::new(file.path())).unwrap();
        assert_eq!(package, Some("cuenv".to_string()));
    }

    #[test]
    fn detect_package_name_handles_missing() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "// only comments").unwrap();
        let package = detect_package_name(Path::new(file.path())).unwrap();
        assert!(package.is_none());
    }

    #[test]
    fn find_env_file_detects_package_mismatch() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("env.cue"), "package other\n").unwrap();

        let status = find_env_file(temp_dir.path(), "cuenv").unwrap();
        match status {
            EnvFileStatus::PackageMismatch { found_package } => {
                assert_eq!(found_package.as_deref(), Some("other"));
            }
            _ => panic!("Expected package mismatch status"),
        }
    }

    #[test]
    fn find_cue_module_root_finds_cue_mod() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        fs::create_dir_all(root.join("cue.mod")).unwrap();

        let nested = root.join("apps/site/src");
        fs::create_dir_all(&nested).unwrap();

        let found = find_cue_module_root(&nested);
        assert!(found.is_some());
        assert_eq!(found.unwrap(), root.canonicalize().unwrap());
    }

    #[test]
    fn find_cue_module_root_returns_none_when_missing() {
        let temp_dir = TempDir::new().unwrap();
        let result = find_cue_module_root(temp_dir.path());
        assert!(result.is_none());
    }

    #[test]
    fn find_ancestor_env_files_collects_all_ancestors() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        fs::create_dir_all(root.join("cue.mod")).unwrap();
        fs::write(root.join("env.cue"), "package cuenv\n").unwrap();

        fs::create_dir_all(root.join("apps")).unwrap();
        fs::write(root.join("apps/env.cue"), "package cuenv\n").unwrap();

        fs::create_dir_all(root.join("apps/site")).unwrap();
        fs::write(root.join("apps/site/env.cue"), "package cuenv\n").unwrap();

        let ancestors = find_ancestor_env_files(&root.join("apps/site"), "cuenv").unwrap();

        assert_eq!(ancestors.len(), 3);
        assert_eq!(ancestors[0], root.canonicalize().unwrap());
        assert_eq!(ancestors[1], root.join("apps").canonicalize().unwrap());
        assert_eq!(ancestors[2], root.join("apps/site").canonicalize().unwrap());
    }

    #[test]
    fn find_ancestor_env_files_stops_at_cue_mod() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        fs::write(root.join("env.cue"), "package cuenv\n").unwrap();

        fs::create_dir_all(root.join("monorepo/cue.mod")).unwrap();
        fs::write(root.join("monorepo/env.cue"), "package cuenv\n").unwrap();

        fs::create_dir_all(root.join("monorepo/apps")).unwrap();
        fs::write(root.join("monorepo/apps/env.cue"), "package cuenv\n").unwrap();

        let ancestors = find_ancestor_env_files(&root.join("monorepo/apps"), "cuenv").unwrap();

        assert_eq!(ancestors.len(), 2);
        assert_eq!(ancestors[0], root.join("monorepo").canonicalize().unwrap());
        assert_eq!(
            ancestors[1],
            root.join("monorepo/apps").canonicalize().unwrap()
        );
    }

    #[test]
    fn find_ancestor_env_files_skips_wrong_package() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        fs::create_dir_all(root.join("cue.mod")).unwrap();
        fs::write(root.join("env.cue"), "package cuenv\n").unwrap();

        fs::create_dir_all(root.join("apps")).unwrap();
        fs::write(root.join("apps/env.cue"), "package other\n").unwrap();

        fs::create_dir_all(root.join("apps/site")).unwrap();
        fs::write(root.join("apps/site/env.cue"), "package cuenv\n").unwrap();

        let ancestors = find_ancestor_env_files(&root.join("apps/site"), "cuenv").unwrap();

        assert_eq!(ancestors.len(), 2);
        assert_eq!(ancestors[0], root.canonicalize().unwrap());
        assert_eq!(ancestors[1], root.join("apps/site").canonicalize().unwrap());
    }

    #[test]
    fn compute_relative_path_basic() {
        let module_root = Path::new("/repo");
        let target = Path::new("/repo/services/api");
        assert_eq!(compute_relative_path(target, module_root), "services/api");
    }

    #[test]
    fn compute_relative_path_same_path() {
        let path = Path::new("/repo");
        assert_eq!(compute_relative_path(path, path), ".");
    }

    #[test]
    fn compute_relative_path_unrelated_paths() {
        let module_root = Path::new("/repo");
        let target = Path::new("/other/path");
        assert_eq!(compute_relative_path(target, module_root), ".");
    }

    #[test]
    fn adjust_meta_key_path_with_dot_slash_prefix() {
        assert_eq!(
            adjust_meta_key_path("./tasks/build", "services/api"),
            "services/api/tasks/build"
        );
    }

    #[test]
    fn adjust_meta_key_path_without_prefix() {
        assert_eq!(
            adjust_meta_key_path("other/path", "services/api"),
            "other/path"
        );
    }

    #[test]
    fn adjust_meta_key_path_only_replaces_first_occurrence() {
        assert_eq!(adjust_meta_key_path("./a/./b", "rel"), "rel/a/./b");
    }

    #[test]
    fn format_eval_errors_empty() {
        let errors: Vec<(PathBuf, String)> = vec![];
        assert_eq!(format_eval_errors(&errors), "");
    }

    #[test]
    fn format_eval_errors_single() {
        let errors = vec![(PathBuf::from("/repo/a"), "syntax error")];
        assert_eq!(format_eval_errors(&errors), "  /repo/a: syntax error");
    }

    #[test]
    fn format_eval_errors_multiple() {
        let errors = vec![
            (PathBuf::from("/repo/a"), "syntax error"),
            (PathBuf::from("/repo/b"), "missing field"),
        ];
        let result = format_eval_errors(&errors);
        assert!(result.contains("/repo/a: syntax error"));
        assert!(result.contains("/repo/b: missing field"));
        assert!(result.contains('\n'));
    }
}
