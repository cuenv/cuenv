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
use glob::glob;
use serde::de::DeserializeOwned;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

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
/// # Implementation Notes
///
/// Patterns are constructed using Path operations to ensure cross-platform compatibility.
/// The root path is not escaped for glob metacharacters, so roots containing `[`, `]`, `?`, `*`,
/// `{`, or `}` may produce unexpected results. For typical workspace roots, this is not an issue.
pub fn resolve_glob_patterns(
    root: &Path,
    patterns: &[String],
    exclusions: &[String],
) -> Result<Vec<PathBuf>> {
    let mut matched_paths = HashSet::new();
    let mut excluded_paths = HashSet::new();

    // Separate inclusion and exclusion patterns
    let mut inclusion_patterns = Vec::new();
    let mut all_exclusions = exclusions.to_vec();

    for pattern in patterns {
        if let Some(stripped) = pattern.strip_prefix('!') {
            all_exclusions.push(stripped.to_string());
        } else {
            inclusion_patterns.push(pattern);
        }
    }

    // Resolve exclusions first
    for pattern in &all_exclusions {
        // Build glob pattern using Path operations for cross-platform compatibility
        let pattern_path = root.join(pattern);
        let pattern_str = pattern_path.to_string_lossy();

        let paths = glob(&pattern_str).map_err(|e| Error::InvalidWorkspaceConfig {
            path: root.to_path_buf(),
            message: format!("Invalid glob pattern '{}': {}", pattern, e),
        })?;

        for entry in paths {
            if let Ok(path) = entry {
                excluded_paths.insert(path);
            }
        }
    }

    // Resolve inclusions
    for pattern in inclusion_patterns {
        // Build glob pattern using Path operations for cross-platform compatibility
        let pattern_path = root.join(pattern);
        let pattern_str = pattern_path.to_string_lossy();

        let paths = glob(&pattern_str).map_err(|e| Error::InvalidWorkspaceConfig {
            path: root.to_path_buf(),
            message: format!("Invalid glob pattern '{}': {}", pattern, e),
        })?;

        for entry in paths {
            if let Ok(path) = entry {
                if path.is_dir() && !excluded_paths.contains(&path) {
                    matched_paths.insert(path);
                }
            }
        }
    }

    let mut result: Vec<PathBuf> = matched_paths.into_iter().collect();
    result.sort();
    Ok(result)
}

/// Reads and parses a JSON file.
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
