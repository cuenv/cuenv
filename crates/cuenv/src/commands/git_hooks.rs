//! Git hook utilities for detecting changed files and matching patterns.
//!
//! This module provides utility functions for git hook operations:
//!
//! - [`find_git_root`] - Find the repository root using gix
//! - [`get_changed_files`] - Get files changed between refs using gix (pure Rust)
//! - [`filter_matching_files`] - Filter files against glob patterns (pure Rust)
//!
//! These functions use the `gix` and `glob` crates for pure-Rust operations,
//! avoiding shell subprocess overhead and shell injection vulnerabilities.
//!
//! ## Design Notes
//!
//! The `get_changed_files` and `filter_matching_files` functions are provided as
//! library functions for pre-push hook implementation. Currently, pre-push hooks
//! use shell scripts for changed file detection and pattern matching. These Rust
//! implementations offer a safer alternative that avoids shell injection risks
//! and provides better performance for large file sets.

use cuenv_core::Result;
#[allow(unused_imports)] // Used for ByteSlice::to_str_lossy
use gix::bstr::ByteSlice;
use std::path::{Path, PathBuf};

/// Get the list of files changed between local and remote refs.
///
/// Uses gix to find the merge-base between local and remote refs
/// and returns the list of files that differ.
///
/// # Arguments
///
/// * `repo_root` - Path to the repository root
/// * `remote` - Remote name (e.g., "origin")
/// * `local_ref` - Local ref to compare (defaults to "HEAD")
/// * `remote_ref` - Remote ref to compare (defaults to tracking branch or main/master)
///
/// # Errors
///
/// Returns an error if the repository cannot be opened or refs cannot be resolved.
// Currently unused - shell script handles this, but this Rust implementation
// provides a safer alternative that avoids shell injection vulnerabilities.
#[allow(dead_code)]
pub fn get_changed_files(
    repo_root: &Path,
    remote: &str,
    local_ref: Option<&str>,
    remote_ref: Option<&str>,
) -> Result<Vec<String>> {
    let repo = gix::open(repo_root)
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to open repository: {e}")))?;

    // Resolve local ref (default to HEAD)
    let local_name = local_ref.unwrap_or("HEAD");
    let local_id = repo
        .rev_parse_single(local_name)
        .map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to resolve '{local_name}': {e}"))
        })?
        .detach();

    // Determine the remote ref to compare against
    let remote_target = if let Some(ref_name) = remote_ref {
        format!("{remote}/{ref_name}")
    } else {
        // Try to find tracking branch, fall back to main/master
        find_remote_target(&repo, remote)
    };

    // Resolve remote ref
    let remote_id = repo
        .rev_parse_single(remote_target.as_str())
        .map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to resolve '{remote_target}': {e}"))
        })?
        .detach();

    // Find merge base
    let merge_bases = repo
        .merge_bases_many(local_id, &[remote_id])
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to find merge base: {e}")))?;

    let base_id = merge_bases.first().map_or(remote_id, |id| id.detach());

    // Get trees for comparison
    let base_commit = repo.find_commit(base_id).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to find base commit: {e}"))
    })?;
    let base_tree = base_commit
        .tree()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to get base tree: {e}")))?;

    let local_commit = repo.find_commit(local_id).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to find local commit: {e}"))
    })?;
    let local_tree = local_commit
        .tree()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to get local tree: {e}")))?;

    // Collect changed file paths
    let mut changed_files = Vec::new();
    base_tree
        .changes()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to initialize diff: {e}")))?
        .for_each_to_obtain_tree(&local_tree, |change| {
            // Get the path from the change
            let path = change.location().to_str_lossy().to_string();
            changed_files.push(path);
            Ok::<_, std::convert::Infallible>(gix::object::tree::diff::Action::Continue)
        })
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to diff trees: {e}")))?;

    Ok(changed_files)
}

/// Find the remote target ref to compare against.
#[allow(dead_code)] // Helper for get_changed_files (currently unused)
fn find_remote_target(repo: &gix::Repository, remote: &str) -> String {
    // Try common branch names
    for branch in ["main", "master"] {
        let ref_name = format!("{remote}/{branch}");
        if repo
            .try_find_reference(ref_name.as_str())
            .ok()
            .flatten()
            .is_some()
        {
            return ref_name;
        }
    }

    // Fall back to origin/main if nothing found
    format!("{remote}/main")
}

/// Find the git repository root directory.
///
/// Uses gix to discover the repository from the given path.
///
/// # Errors
///
/// Returns an error if not in a git repository.
pub fn find_git_root(start_path: &Path) -> Result<PathBuf> {
    let repo = gix::discover(start_path)
        .map_err(|e| cuenv_core::Error::configuration(format!("Not in a git repository: {e}")))?;

    // Get the working directory (workdir) which is the repository root
    let workdir = repo
        .workdir()
        .ok_or_else(|| cuenv_core::Error::configuration("Cannot operate in a bare repository"))?;

    Ok(workdir.to_path_buf())
}

/// Filter files that match any of the input patterns.
///
/// # Arguments
/// * `input_patterns` - Glob patterns to match against (e.g., `["src/**/*.rs", "*.toml"]`)
/// * `changed_files` - List of file paths to filter
/// * `repo_root` - Repository root for resolving directory patterns
///
/// # Returns
/// Files from `changed_files` that match at least one pattern.
/// If `input_patterns` is empty, all files are returned.
///
/// # Pattern Handling
/// - `src/**` → matches all files under `src/` (already valid glob, used as-is)
/// - `src/` → converted to `src/**/*` to match all files under the directory
/// - `*.rs` → matches files ending in `.rs`
/// - Plain paths that are directories → treated as `dir/**/*`
///
/// # Errors
/// Returns an error if any pattern is an invalid glob.
// Currently unused - shell script uses grep for filtering, but this Rust
// implementation provides safer glob matching without shell injection risks.
#[allow(dead_code)]
pub fn filter_matching_files(
    input_patterns: &[String],
    changed_files: &[String],
    repo_root: &Path,
) -> Result<Vec<String>> {
    use glob::Pattern;

    // If no input patterns specified, all changed files match
    if input_patterns.is_empty() {
        return Ok(changed_files.to_vec());
    }

    let mut matching = Vec::new();

    for file in changed_files {
        for pattern in input_patterns {
            // Normalize glob patterns for directory matching
            let normalized_pattern = if pattern.ends_with("/**") {
                // Already a valid directory glob - use as-is
                pattern.clone()
            } else if pattern.ends_with('/') {
                // Directory path ending with / - add glob suffix
                format!("{}**/*", pattern)
            } else if !pattern.contains('*') && !pattern.contains('?') {
                // Plain path - could be a directory or file
                // Check if it's a directory in the repo
                let full_path = repo_root.join(pattern);
                if full_path.is_dir() {
                    format!("{}/**/*", pattern)
                } else {
                    pattern.clone()
                }
            } else {
                pattern.clone()
            };

            let glob_pattern = Pattern::new(&normalized_pattern).map_err(|e| {
                cuenv_core::Error::configuration(format!(
                    "Invalid glob pattern '{}': {}",
                    pattern, e
                ))
            })?;

            if glob_pattern.matches(file) {
                matching.push(file.clone());
                break; // File matched, no need to check more patterns
            }
        }
    }

    Ok(matching)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_filter_matching_files_empty_patterns() {
        let patterns: Vec<String> = vec![];
        let files = vec!["src/main.rs".to_string(), "Cargo.toml".to_string()];
        let temp = TempDir::new().unwrap();

        let result = filter_matching_files(&patterns, &files, temp.path()).unwrap();
        assert_eq!(result, files);
    }

    #[test]
    fn test_filter_matching_files_glob_pattern() {
        let patterns = vec!["src/**/*.rs".to_string()];
        let files = vec![
            "src/main.rs".to_string(),
            "src/lib/utils.rs".to_string(),
            "Cargo.toml".to_string(),
            "README.md".to_string(),
        ];
        let temp = TempDir::new().unwrap();

        let result = filter_matching_files(&patterns, &files, temp.path()).unwrap();
        assert_eq!(result, vec!["src/main.rs", "src/lib/utils.rs"]);
    }

    #[test]
    fn test_filter_matching_files_exact_match() {
        let patterns = vec!["Cargo.toml".to_string()];
        let files = vec![
            "src/main.rs".to_string(),
            "Cargo.toml".to_string(),
            "README.md".to_string(),
        ];
        let temp = TempDir::new().unwrap();

        let result = filter_matching_files(&patterns, &files, temp.path()).unwrap();
        assert_eq!(result, vec!["Cargo.toml"]);
    }

    #[test]
    fn test_filter_matching_files_multiple_patterns() {
        let patterns = vec!["src/**/*.rs".to_string(), "*.toml".to_string()];
        let files = vec![
            "src/main.rs".to_string(),
            "Cargo.toml".to_string(),
            "README.md".to_string(),
        ];
        let temp = TempDir::new().unwrap();

        let result = filter_matching_files(&patterns, &files, temp.path()).unwrap();
        assert_eq!(result, vec!["src/main.rs", "Cargo.toml"]);
    }

    #[test]
    fn test_filter_matching_files_directory_with_trailing_slash() {
        // Pattern ending with / should match all files under that directory
        let patterns = vec!["src/".to_string()];
        let files = vec![
            "src/main.rs".to_string(),
            "src/lib/utils.rs".to_string(),
            "Cargo.toml".to_string(),
        ];
        let temp = TempDir::new().unwrap();

        let result = filter_matching_files(&patterns, &files, temp.path()).unwrap();
        assert_eq!(result, vec!["src/main.rs", "src/lib/utils.rs"]);
    }

    #[test]
    fn test_filter_matching_files_directory_glob_suffix() {
        // Pattern ending with /** should match all files under that directory
        let patterns = vec!["src/**".to_string()];
        let files = vec![
            "src/main.rs".to_string(),
            "src/lib/utils.rs".to_string(),
            "Cargo.toml".to_string(),
        ];
        let temp = TempDir::new().unwrap();

        let result = filter_matching_files(&patterns, &files, temp.path()).unwrap();
        assert_eq!(result, vec!["src/main.rs", "src/lib/utils.rs"]);
    }

    #[test]
    fn test_filter_matching_files_invalid_pattern_returns_error() {
        // Invalid glob pattern should return an error
        let patterns = vec!["[invalid".to_string()];
        let files = vec!["src/main.rs".to_string()];
        let temp = TempDir::new().unwrap();

        let result = filter_matching_files(&patterns, &files, temp.path());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Invalid glob pattern"));
    }
}
