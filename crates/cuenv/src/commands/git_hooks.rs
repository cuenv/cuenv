//! Git hook utilities for detecting changed files and matching patterns.
//!
//! This module provides utility functions used by the git-hooks sync provider
//! to detect files changed between refs and filter them against patterns.

use cuenv_core::Result;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Get the list of files changed between local and remote refs.
///
/// This runs `git diff --name-only` between the local and remote refs to get
/// the list of files that will be pushed.
pub fn get_changed_files(
    repo_root: &Path,
    remote: &str,
    local_ref: Option<&str>,
    remote_ref: Option<&str>,
) -> Result<Vec<String>> {
    let local = local_ref.unwrap_or("HEAD");

    // Determine the remote ref to compare against
    // If remote_ref is provided, use it directly
    // Otherwise, try to get the upstream tracking branch
    let remote_target = if let Some(ref_name) = remote_ref {
        format!("{remote}/{ref_name}")
    } else {
        // Try to get the upstream tracking branch for HEAD
        let upstream_output = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "@{upstream}"])
            .current_dir(repo_root)
            .output();

        match upstream_output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            _ => {
                // Fall back to remote/main or remote/master
                let main_exists = Command::new("git")
                    .args(["rev-parse", "--verify", &format!("{remote}/main")])
                    .current_dir(repo_root)
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);

                if main_exists {
                    format!("{remote}/main")
                } else {
                    format!("{remote}/master")
                }
            }
        }
    };

    // Get the merge base between local and remote
    let merge_base_output = Command::new("git")
        .args(["merge-base", local, &remote_target])
        .current_dir(repo_root)
        .output()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to run git merge-base: {e}")))?;

    let base_ref = if merge_base_output.status.success() {
        String::from_utf8_lossy(&merge_base_output.stdout)
            .trim()
            .to_string()
    } else {
        // If merge-base fails (e.g., no common ancestor), compare directly
        remote_target.clone()
    };

    // Get changed files between base and local
    let diff_output = Command::new("git")
        .args(["diff", "--name-only", &base_ref, local])
        .current_dir(repo_root)
        .output()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to run git diff: {e}")))?;

    if !diff_output.status.success() {
        let stderr = String::from_utf8_lossy(&diff_output.stderr);
        return Err(cuenv_core::Error::configuration(format!(
            "git diff failed: {stderr}"
        )));
    }

    let files: Vec<String> = String::from_utf8_lossy(&diff_output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect();

    Ok(files)
}

/// Find the git repository root directory.
pub fn find_git_root(start_path: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(start_path)
        .output()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to run git: {e}")))?;

    if !output.status.success() {
        return Err(cuenv_core::Error::configuration(
            "Not in a git repository".to_string(),
        ));
    }

    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(PathBuf::from(root))
}

/// Filter files that match any of the input patterns.
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
            // Handle glob patterns
            let normalized_pattern = if pattern.ends_with('/') || pattern.ends_with("/**") {
                // Directory pattern - match any file inside
                format!("{}**/*", pattern.trim_end_matches("**/*").trim_end_matches('/'))
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

            match Pattern::new(&normalized_pattern) {
                Ok(glob_pattern) => {
                    if glob_pattern.matches(file) {
                        matching.push(file.clone());
                        break; // File matched, no need to check more patterns
                    }
                }
                Err(_) => {
                    // If pattern is invalid, try exact match
                    if file == pattern || file.starts_with(&format!("{pattern}/")) {
                        matching.push(file.clone());
                        break;
                    }
                }
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
}
