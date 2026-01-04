//! Affected detection utilities for tasks and files.
//!
//! This module provides utilities for determining if tasks are affected by file changes,
//! used by CI pipelines and incremental builds.
//!
//! # Design Philosophy
//!
//! - Tasks with inputs are affected if any input pattern matches changed files
//! - Tasks with NO inputs are always considered affected (we can't determine what affects them)
//! - This is the safe behavior - if we can't determine, we run the task

use globset::{Glob, GlobSet, GlobSetBuilder};
use std::path::{Path, PathBuf};

/// Check if any of the given files match a pattern.
///
/// Supports two matching modes:
/// - **Simple paths**: Patterns without wildcards (`*`, `?`, `[`) are treated as path prefixes.
///   For example, `"crates"` matches `"crates/foo/bar.rs"`.
/// - **Glob patterns**: Patterns with wildcards use glob matching.
///
/// # Arguments
///
/// * `files` - Changed file paths (typically repo-relative from git diff)
/// * `project_root` - The project root for normalizing paths
/// * `pattern` - The input pattern to match against
///
/// # Returns
///
/// `true` if any file matches the pattern
pub fn matches_pattern(files: &[PathBuf], project_root: &Path, pattern: &str) -> bool {
    // If pattern doesn't contain glob characters, treat it as a path prefix
    let is_simple_path = !pattern.contains('*') && !pattern.contains('?') && !pattern.contains('[');

    for file in files {
        // Normalize the file path relative to project root
        let relative_path = normalize_path(file, project_root);

        if is_simple_path {
            // Check if the pattern is a prefix of the file path
            let pattern_path = Path::new(pattern);
            if relative_path.starts_with(pattern_path) {
                return true;
            }
        } else {
            // Use glob matching for patterns with wildcards
            let Ok(glob) = Glob::new(pattern) else {
                tracing::trace!(pattern, "Skipping invalid glob pattern");
                continue;
            };
            let Ok(set) = GlobSetBuilder::new().add(glob).build() else {
                continue;
            };
            if set.is_match(&relative_path) {
                return true;
            }
        }
    }

    false
}

/// Build a GlobSet from multiple patterns for efficient batch matching.
///
/// # Arguments
///
/// * `patterns` - Iterator of glob patterns
///
/// # Returns
///
/// A compiled GlobSet, or None if no valid patterns
pub fn build_glob_set<'a>(patterns: impl Iterator<Item = &'a str>) -> Option<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    let mut has_patterns = false;

    for pattern in patterns {
        if let Ok(glob) = Glob::new(pattern) {
            builder.add(glob);
            has_patterns = true;
        } else {
            tracing::trace!(pattern, "Skipping invalid glob pattern");
        }
    }

    if !has_patterns {
        return None;
    }

    builder.build().ok()
}

/// Normalize a file path relative to a project root.
///
/// Handles the common case where git diff returns repo-relative paths,
/// but we need to match against project-relative patterns.
fn normalize_path(file: &Path, project_root: &Path) -> PathBuf {
    // If root is "." or empty, use file as-is
    if project_root == Path::new(".") || project_root.as_os_str().is_empty() {
        return file.to_path_buf();
    }

    // If file is already relative (e.g., from git diff), check if it starts with project root
    if file.is_relative() {
        // Try to strip the project root prefix from the file
        if let Ok(stripped) = file.strip_prefix(project_root) {
            return stripped.to_path_buf();
        }
        // File is relative but doesn't start with project root - use as-is
        return file.to_path_buf();
    }

    // File is absolute - strip the project root prefix
    file.strip_prefix(project_root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| file.to_path_buf())
}

/// Trait for types that can determine if they are affected by file changes.
///
/// Implement this trait on task types to enable affected detection in
/// CI pipelines and incremental builds.
pub trait AffectedBy {
    /// Returns true if this item is affected by the given file changes.
    ///
    /// # Arguments
    ///
    /// * `changed_files` - Paths of files that have changed (typically repo-relative)
    /// * `project_root` - Root path of the project containing this item
    ///
    /// # Returns
    ///
    /// `true` if the item should be considered affected and needs to run
    fn is_affected_by(&self, changed_files: &[PathBuf], project_root: &Path) -> bool;

    /// Returns the input patterns that determine what affects this item.
    ///
    /// Used for debugging and reporting which patterns matched.
    fn input_patterns(&self) -> Vec<&str>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_pattern_simple_prefix() {
        let files = vec![PathBuf::from("crates/foo/bar.rs")];
        let root = Path::new(".");

        assert!(matches_pattern(&files, root, "crates"));
        assert!(matches_pattern(&files, root, "crates/foo"));
        assert!(matches_pattern(&files, root, "crates/foo/bar.rs"));
    }

    #[test]
    fn test_matches_pattern_no_match() {
        let files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");

        assert!(!matches_pattern(&files, root, "crates"));
        assert!(!matches_pattern(&files, root, "tests"));
    }

    #[test]
    fn test_matches_pattern_glob() {
        let files = vec![PathBuf::from("src/lib.rs"), PathBuf::from("src/main.rs")];
        let root = Path::new(".");

        assert!(matches_pattern(&files, root, "src/*.rs"));
        assert!(!matches_pattern(&files, root, "*.txt"));
    }

    #[test]
    fn test_matches_pattern_with_project_root() {
        // File is repo-relative, project root is a subdirectory
        let files = vec![PathBuf::from("projects/website/src/app.rs")];
        let root = Path::new("projects/website");

        // Pattern is project-relative
        assert!(matches_pattern(&files, root, "src"));
        assert!(matches_pattern(&files, root, "src/app.rs"));
    }

    #[test]
    fn test_matches_pattern_different_project() {
        // File is in a different project
        let files = vec![PathBuf::from("projects/api/src/main.rs")];
        let root = Path::new("projects/website");

        // Should not match - file is in api, not website
        assert!(!matches_pattern(&files, root, "src"));
    }

    #[test]
    fn test_normalize_path_relative_file_with_project_root() {
        let file = Path::new("projects/website/src/lib.rs");
        let root = Path::new("projects/website");

        let normalized = normalize_path(file, root);
        assert_eq!(normalized, PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn test_normalize_path_dot_root() {
        let file = Path::new("src/lib.rs");
        let root = Path::new(".");

        let normalized = normalize_path(file, root);
        assert_eq!(normalized, PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn test_build_glob_set() {
        let patterns = ["src/**/*.rs", "tests/*.rs"];
        let set = build_glob_set(patterns.iter().copied()).unwrap();

        assert!(set.is_match("src/lib.rs"));
        assert!(set.is_match("src/foo/bar.rs"));
        assert!(set.is_match("tests/test.rs"));
        assert!(!set.is_match("docs/readme.md"));
    }

    #[test]
    fn test_build_glob_set_invalid_patterns() {
        let patterns = ["[invalid", "src/**"];
        let set = build_glob_set(patterns.iter().copied()).unwrap();

        // Invalid pattern is skipped, valid one still works
        assert!(set.is_match("src/lib.rs"));
    }

    #[test]
    fn test_build_glob_set_empty() {
        let patterns: [&str; 0] = [];
        let set = build_glob_set(patterns.iter().copied());
        assert!(set.is_none());
    }
}
