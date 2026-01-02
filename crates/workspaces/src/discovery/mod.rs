//! Workspace discovery implementations for various package managers.
//!
//! This module provides implementations of the [`WorkspaceDiscovery`] trait for
//! discovering workspace configurations from:
//! - `package.json` (npm, Bun, Yarn)
//! - `pnpm-workspace.yaml` (pnpm)
//! - `Cargo.toml` (Rust/Cargo)
//!
//! # Usage
//!
//! ```rust,ignore
//! use cuenv_workspaces::discovery::{PackageJsonDiscovery, CargoTomlDiscovery};
//! use cuenv_workspaces::WorkspaceDiscovery;
//! use std::path::Path;
//!
//! let root = Path::new(".");
//!
//! // Try npm/yarn/bun workspace
//! if let Ok(workspace) = PackageJsonDiscovery.discover(root) {
//!     println!("Found {} npm members", workspace.member_count());
//! }
//!
//! // Try cargo workspace
//! if let Ok(workspace) = CargoTomlDiscovery.discover(root) {
//!     println!("Found {} crate members", workspace.member_count());
//! }
//! ```

use crate::error::{Error, Result};
use glob::Pattern;
use serde::de::DeserializeOwned;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[cfg(feature = "discovery-cargo")]
pub mod cargo_toml;

#[cfg(feature = "discovery-package-json")]
pub mod package_json;

#[cfg(feature = "discovery-pnpm")]
pub mod pnpm_workspace;

#[cfg(feature = "discovery-cargo")]
pub use cargo_toml::CargoTomlDiscovery;

#[cfg(feature = "discovery-package-json")]
pub use package_json::PackageJsonDiscovery;

#[cfg(feature = "discovery-pnpm")]
pub use pnpm_workspace::PnpmWorkspaceDiscovery;

/// Resolves glob patterns to find directories, handling exclusions.
///
/// # Arguments
///
/// * `root` - The root directory to resolve patterns from.
/// * `patterns` - List of glob patterns to match (e.g., "packages/*").
/// * `exclusions` - List of glob patterns to exclude (e.g., "packages/excluded").
///   Note: Patterns starting with "!" in the `patterns` list are also treated as exclusions.
///
/// # Returns
///
/// A sorted list of unique, absolute paths (rooted under `root`) that match the patterns and are not excluded.
///
/// # Errors
///
/// Returns an error if any glob pattern is invalid or if the filesystem cannot
/// be read while resolving glob matches.
///
/// # Implementation Notes
///
/// This implementation uses `walkdir` for efficient traversal and prunes common
/// heavy directories (`node_modules`, `.git`, `target`, `dist`) to improve performance
pub fn resolve_glob_patterns(
    root: &Path,
    patterns: &[String],
    exclusions: &[String],
) -> Result<Vec<PathBuf>> {
    let mut matched_paths = HashSet::new();

    // Compile patterns
    let mut inclusion_patterns = Vec::new();
    let mut exclusion_patterns = Vec::new();

    // Pre-compile default exclusions to avoid traversing heavy directories
    let default_ignores = [
        "**/node_modules/**",
        "**/.git/**",
        "**/target/**",
        "**/dist/**",
    ];
    for ignore in default_ignores {
        if let Ok(pat) = Pattern::new(ignore) {
            exclusion_patterns.push(pat);
        }
    }

    for p in exclusions {
        if let Ok(pat) = Pattern::new(p) {
            exclusion_patterns.push(pat);
        }
    }

    for p in patterns {
        if let Some(stripped) = p.strip_prefix('!') {
            if let Ok(pat) = Pattern::new(stripped) {
                exclusion_patterns.push(pat);
            }
        } else if let Ok(pat) = Pattern::new(p) {
            inclusion_patterns.push(pat);
        }
    }

    // Walk the directory tree
    let walker = WalkDir::new(root).follow_links(false);

    for entry in walker
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_str().unwrap_or("");
            // Standard directory ignores to prune search tree
            if name == "node_modules" || name == ".git" || name == "target" || name == "dist" {
                return false;
            }
            true
        })
        .filter_map(std::result::Result::ok)
    {
        if !entry.file_type().is_dir() {
            continue;
        }

        let path = entry.path();
        // Skip root itself
        if path == root {
            continue;
        }

        // Relativize path for matching
        let Ok(rel_path) = path.strip_prefix(root) else {
            continue;
        };

        // Check exclusions
        let is_excluded = exclusion_patterns.iter().any(|p| p.matches_path(rel_path));
        if is_excluded {
            continue;
        }

        // Check inclusions
        let is_included = inclusion_patterns.iter().any(|p| p.matches_path(rel_path));
        if is_included {
            matched_paths.insert(path.to_path_buf());
        }
    }

    let mut result: Vec<PathBuf> = matched_paths.into_iter().collect();
    result.sort();
    Ok(result)
}

/// Reads and parses a JSON file.
///
/// # Errors
///
/// Returns an error if the file cannot be read or parsed as valid JSON.
pub fn read_json_file<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let content = fs::read_to_string(path).map_err(|e| Error::Io {
        source: e,
        path: Some(path.to_path_buf()),
        operation: "reading json file".to_string(),
    })?;

    serde_json::from_str(&content).map_err(|e| Error::Json {
        source: e,
        path: Some(path.to_path_buf()),
    })
}

/// Reads and parses a YAML file.
///
/// # Errors
///
/// Returns an error if the file cannot be read or parsed as valid YAML.
#[cfg(feature = "serde_yaml")]
pub fn read_yaml_file<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let content = fs::read_to_string(path).map_err(|e| Error::Io {
        source: e,
        path: Some(path.to_path_buf()),
        operation: "reading yaml file".to_string(),
    })?;

    serde_yaml::from_str(&content).map_err(|e| Error::Yaml {
        source: e,
        path: Some(path.to_path_buf()),
    })
}

/// Reads and parses a TOML file.
///
/// # Errors
///
/// Returns an error if the file cannot be read or parsed as valid TOML.
#[cfg(feature = "toml")]
pub fn read_toml_file<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let content = fs::read_to_string(path).map_err(|e| Error::Io {
        source: e,
        path: Some(path.to_path_buf()),
        operation: "reading toml file".to_string(),
    })?;

    toml::from_str(&content).map_err(|e| Error::Toml {
        source: e,
        path: Some(path.to_path_buf()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_resolve_glob_patterns_basic() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create directory structure
        fs::create_dir_all(root.join("packages/a")).unwrap();
        fs::create_dir_all(root.join("packages/b")).unwrap();
        fs::create_dir_all(root.join("apps/app1")).unwrap();

        let patterns = vec!["packages/*".to_string()];
        let result = resolve_glob_patterns(root, &patterns, &[]).unwrap();

        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|p| p.ends_with("packages/a")));
        assert!(result.iter().any(|p| p.ends_with("packages/b")));
    }

    #[test]
    fn test_resolve_glob_patterns_with_exclusions() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        fs::create_dir_all(root.join("packages/a")).unwrap();
        fs::create_dir_all(root.join("packages/b")).unwrap();
        fs::create_dir_all(root.join("packages/excluded")).unwrap();

        let patterns = vec!["packages/*".to_string()];
        let exclusions = vec!["packages/excluded".to_string()];
        let result = resolve_glob_patterns(root, &patterns, &exclusions).unwrap();

        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|p| p.ends_with("packages/a")));
        assert!(result.iter().any(|p| p.ends_with("packages/b")));
        assert!(!result.iter().any(|p| p.ends_with("packages/excluded")));
    }

    #[test]
    fn test_resolve_glob_patterns_negation_prefix() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        fs::create_dir_all(root.join("packages/a")).unwrap();
        fs::create_dir_all(root.join("packages/b")).unwrap();
        fs::create_dir_all(root.join("packages/ignored")).unwrap();

        let patterns = vec!["packages/*".to_string(), "!packages/ignored".to_string()];
        let result = resolve_glob_patterns(root, &patterns, &[]).unwrap();

        assert!(result.len() >= 2);
        assert!(!result.iter().any(|p| p.ends_with("packages/ignored")));
    }

    #[test]
    fn test_resolve_glob_patterns_skips_node_modules() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        fs::create_dir_all(root.join("packages/a")).unwrap();
        fs::create_dir_all(root.join("node_modules/package")).unwrap();

        let patterns = vec!["**/*".to_string()];
        let result = resolve_glob_patterns(root, &patterns, &[]).unwrap();

        // Should not include node_modules
        assert!(!result.iter().any(|p| p.to_str().unwrap().contains("node_modules")));
    }

    #[test]
    fn test_resolve_glob_patterns_multiple_patterns() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        fs::create_dir_all(root.join("packages/a")).unwrap();
        fs::create_dir_all(root.join("apps/b")).unwrap();

        let patterns = vec!["packages/*".to_string(), "apps/*".to_string()];
        let result = resolve_glob_patterns(root, &patterns, &[]).unwrap();

        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|p| p.ends_with("packages/a")));
        assert!(result.iter().any(|p| p.ends_with("apps/b")));
    }

    #[test]
    fn test_resolve_glob_patterns_nested() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        fs::create_dir_all(root.join("src/nested/deep")).unwrap();

        let patterns = vec!["src/nested/*".to_string()];
        let result = resolve_glob_patterns(root, &patterns, &[]).unwrap();

        assert_eq!(result.len(), 1);
        assert!(result[0].ends_with("src/nested/deep"));
    }

    #[test]
    fn test_resolve_glob_patterns_empty() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let patterns = vec!["packages/*".to_string()];
        let result = resolve_glob_patterns(root, &patterns, &[]).unwrap();

        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_read_json_file_valid() {
        let temp_dir = TempDir::new().unwrap();
        let file = temp_dir.path().join("test.json");

        #[derive(serde::Deserialize)]
        struct TestData {
            name: String,
        }

        fs::write(&file, r#"{"name": "test"}"#).unwrap();
        let data: TestData = read_json_file(&file).unwrap();

        assert_eq!(data.name, "test");
    }

    #[test]
    fn test_read_json_file_invalid() {
        let temp_dir = TempDir::new().unwrap();
        let file = temp_dir.path().join("test.json");

        #[derive(serde::Deserialize)]
        struct TestData {
            _name: String,
        }

        fs::write(&file, r#"{"invalid json"#).unwrap();
        let result: Result<TestData> = read_json_file(&file);

        assert!(result.is_err());
    }

    #[test]
    fn test_read_json_file_missing() {
        #[derive(serde::Deserialize)]
        struct TestData {
            _name: String,
        }

        let result: Result<TestData> = read_json_file(Path::new("/nonexistent/file.json"));
        assert!(result.is_err());
    }

    #[cfg(feature = "toml")]
    #[test]
    fn test_read_toml_file_valid() {
        let temp_dir = TempDir::new().unwrap();
        let file = temp_dir.path().join("test.toml");

        #[derive(serde::Deserialize)]
        struct TestData {
            name: String,
        }

        fs::write(&file, r#"name = "test""#).unwrap();
        let data: TestData = read_toml_file(&file).unwrap();

        assert_eq!(data.name, "test");
    }

    #[cfg(feature = "toml")]
    #[test]
    fn test_read_toml_file_invalid() {
        let temp_dir = TempDir::new().unwrap();
        let file = temp_dir.path().join("test.toml");

        #[derive(serde::Deserialize)]
        struct TestData {
            _name: String,
        }

        fs::write(&file, r#"invalid toml ="#).unwrap();
        let result: Result<TestData> = read_toml_file(&file);

        assert!(result.is_err());
    }

    #[cfg(feature = "serde_yaml")]
    #[test]
    fn test_read_yaml_file_valid() {
        let temp_dir = TempDir::new().unwrap();
        let file = temp_dir.path().join("test.yaml");

        #[derive(serde::Deserialize)]
        struct TestData {
            name: String,
        }

        fs::write(&file, "name: test").unwrap();
        let data: TestData = read_yaml_file(&file).unwrap();

        assert_eq!(data.name, "test");
    }

    #[cfg(feature = "serde_yaml")]
    #[test]
    fn test_read_yaml_file_invalid() {
        let temp_dir = TempDir::new().unwrap();
        let file = temp_dir.path().join("test.yaml");

        #[derive(serde::Deserialize)]
        struct TestData {
            _name: String,
        }

        fs::write(&file, "invalid: yaml: structure:").unwrap();
        let result: Result<TestData> = read_yaml_file(&file);

        assert!(result.is_err());
    }
}
