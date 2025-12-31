//! env.cue file detection and package validation.
//!
//! Provides utilities for finding and validating env.cue files
//! within a CUE module hierarchy.

use cuenv_core::{Error, Result};
use std::fs;
use std::path::{Path, PathBuf};

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
    let directory = if path.is_absolute() {
        path.to_path_buf()
    } else {
        PathBuf::from(path)
    };

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

fn detect_package_name(env_file: &Path) -> Result<Option<String>> {
    let contents = fs::read_to_string(env_file).map_err(|e| {
        Error::configuration(format!(
            "Failed to read env.cue ({}): {e}",
            env_file.display()
        ))
    })?;

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
    let mut current = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start)
    };

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
    let start_canonical = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| Error::configuration(format!("Failed to get current directory: {e}")))?
            .join(start)
    }
    .canonicalize()
    .map_err(|e| Error::configuration(format!("Failed to canonicalize path: {e}")))?;

    // Find the module root (stopping point)
    let module_root = find_cue_module_root(&start_canonical);

    let mut ancestors = Vec::new();
    let mut current = start_canonical;

    loop {
        // Check if this directory has an env.cue with matching package
        if let EnvFileStatus::Match(dir) = find_env_file(&current, expected_package)? {
            ancestors.push(dir);
        }

        // Stop if we've reached the module root
        if let Some(ref root) = module_root
            && current == *root
        {
            break;
        }

        // Move to parent
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
    }

    // Reverse to get root-to-leaf order (ancestors first)
    ancestors.reverse();
    Ok(ancestors)
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
        EnvFileStatus, detect_package_name, find_ancestor_env_files, find_cue_module_root,
        find_env_file, strip_comments,
    };
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use tempfile::{NamedTempFile, TempDir};

    #[test]
    fn strip_comments_removes_line_and_block_comments() {
        let source = r"
// line comment
/* block
comment */
package cuenv // inline
        ";
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

        // Create cue.mod at root
        fs::create_dir_all(root.join("cue.mod")).unwrap();

        // Create nested directories
        let nested = root.join("apps/site/src");
        fs::create_dir_all(&nested).unwrap();

        // Find module root from nested dir
        let found = find_cue_module_root(&nested);
        assert!(found.is_some());
        assert_eq!(found.unwrap(), root.canonicalize().unwrap());
    }

    #[test]
    fn find_cue_module_root_returns_none_when_missing() {
        let temp_dir = TempDir::new().unwrap();
        // No cue.mod directory
        let result = find_cue_module_root(temp_dir.path());
        assert!(result.is_none());
    }

    #[test]
    fn find_ancestor_env_files_collects_all_ancestors() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create structure:
        // root/
        //   cue.mod/
        //   env.cue (package cuenv)
        //   apps/
        //     env.cue (package cuenv)
        //     site/
        //       env.cue (package cuenv)
        fs::create_dir_all(root.join("cue.mod")).unwrap();
        fs::write(root.join("env.cue"), "package cuenv\n").unwrap();

        fs::create_dir_all(root.join("apps")).unwrap();
        fs::write(root.join("apps/env.cue"), "package cuenv\n").unwrap();

        fs::create_dir_all(root.join("apps/site")).unwrap();
        fs::write(root.join("apps/site/env.cue"), "package cuenv\n").unwrap();

        // Find ancestors from apps/site
        let ancestors = find_ancestor_env_files(&root.join("apps/site"), "cuenv").unwrap();

        // Should return [root, apps, apps/site] in root-to-leaf order
        assert_eq!(ancestors.len(), 3);
        assert_eq!(ancestors[0], root.canonicalize().unwrap());
        assert_eq!(ancestors[1], root.join("apps").canonicalize().unwrap());
        assert_eq!(ancestors[2], root.join("apps/site").canonicalize().unwrap());
    }

    #[test]
    fn find_ancestor_env_files_stops_at_cue_mod() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create structure with cue.mod in middle:
        // root/
        //   env.cue (should NOT be included - outside module)
        //   monorepo/
        //     cue.mod/
        //     env.cue (should be included)
        //     apps/
        //       env.cue (should be included)
        fs::write(root.join("env.cue"), "package cuenv\n").unwrap();

        fs::create_dir_all(root.join("monorepo/cue.mod")).unwrap();
        fs::write(root.join("monorepo/env.cue"), "package cuenv\n").unwrap();

        fs::create_dir_all(root.join("monorepo/apps")).unwrap();
        fs::write(root.join("monorepo/apps/env.cue"), "package cuenv\n").unwrap();

        // Find ancestors from monorepo/apps
        let ancestors = find_ancestor_env_files(&root.join("monorepo/apps"), "cuenv").unwrap();

        // Should only return [monorepo, monorepo/apps] - not root
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

        // Create structure with mixed packages:
        // root/
        //   cue.mod/
        //   env.cue (package cuenv)
        //   apps/
        //     env.cue (package other) - should be skipped
        //     site/
        //       env.cue (package cuenv)
        fs::create_dir_all(root.join("cue.mod")).unwrap();
        fs::write(root.join("env.cue"), "package cuenv\n").unwrap();

        fs::create_dir_all(root.join("apps")).unwrap();
        fs::write(root.join("apps/env.cue"), "package other\n").unwrap();

        fs::create_dir_all(root.join("apps/site")).unwrap();
        fs::write(root.join("apps/site/env.cue"), "package cuenv\n").unwrap();

        let ancestors = find_ancestor_env_files(&root.join("apps/site"), "cuenv").unwrap();

        // Should return [root, apps/site] - skipping apps (wrong package)
        assert_eq!(ancestors.len(), 2);
        assert_eq!(ancestors[0], root.canonicalize().unwrap());
        assert_eq!(ancestors[1], root.join("apps/site").canonicalize().unwrap());
    }
}
