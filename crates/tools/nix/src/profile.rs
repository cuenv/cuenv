//! Nix profile management for hermetic tool execution.
//!
//! Each cuenv project gets a dedicated Nix profile in the XDG cache directory.
//! This ensures full Nix closures are available, not just individual binaries.

use crate::commands;
use cuenv_core::lockfile::Lockfile;
use cuenv_core::paths::cache_dir;
use cuenv_core::tools::Platform;
use cuenv_core::{Error, Result};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::info;

/// Get the profile path for a project.
///
/// Profiles are stored at `$XDG_CACHE_HOME/cuenv/nix-profiles/{project-hash}/`
///
/// # Errors
///
/// Returns an error if the cache directory cannot be determined.
pub fn profile_path_for_project(project_root: &Path) -> Result<PathBuf> {
    let cache = cache_dir()?;
    let project_id = project_profile_id(project_root);
    Ok(cache.join("nix-profiles").join(project_id))
}

/// Compute a stable identifier for a project based on its canonical path.
///
/// Uses a 16-character hex prefix of the SHA256 hash of the canonical path.
fn project_profile_id(project_root: &Path) -> String {
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// Ensure profile contains all Nix tools from lockfile for current platform.
///
/// This function is idempotent - it only installs packages that are not
/// already present in the profile.
///
/// # Errors
///
/// Returns an error if profile creation fails or package installation fails.
pub async fn ensure_profile(project_root: &Path, lockfile: &Lockfile) -> Result<PathBuf> {
    let profile_path = profile_path_for_project(project_root)?;
    let platform = Platform::current().to_string();

    // Collect Nix tools for current platform
    let nix_tools: Vec<_> = lockfile
        .tools
        .iter()
        .filter_map(|(name, tool)| {
            tool.platforms
                .get(&platform)
                .filter(|p| p.provider == "nix")
                .map(|p| (name.clone(), p.clone()))
        })
        .collect();

    if nix_tools.is_empty() {
        return Ok(profile_path);
    }

    // Create profile directory parent
    if let Some(parent) = profile_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Get currently installed packages
    let installed = get_installed_flake_refs(&profile_path)
        .await
        .unwrap_or_default();

    // Install missing packages
    for (name, platform_data) in &nix_tools {
        let flake = platform_data
            .source
            .get("flake")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::configuration("Missing flake in Nix tool source"))?;
        let package = platform_data
            .source
            .get("package")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::configuration("Missing package in Nix tool source"))?;

        let flake_ref = format!("{flake}#{package}");

        if !installed.contains(&flake_ref) {
            info!(tool = %name, %flake_ref, "Installing into Nix profile");
            commands::profile_install(&profile_path, &flake_ref).await?;
        }
    }

    Ok(profile_path)
}

/// Get flake refs currently installed in a profile.
async fn get_installed_flake_refs(profile_path: &Path) -> Result<HashSet<String>> {
    let json = commands::profile_list(profile_path).await?;

    // Parse JSON and extract flake references
    // Profile JSON structure: { "elements": [ { "originalUrl": "..." }, ... ] }
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();

    let mut refs = HashSet::new();
    if let Some(elements) = parsed.get("elements").and_then(|e| e.as_array()) {
        for elem in elements {
            if let Some(url) = elem.get("originalUrl").and_then(|u| u.as_str()) {
                refs.insert(url.to_string());
            }
        }
    }
    Ok(refs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_project_profile_id_is_deterministic() {
        let path = Path::new("/home/user/project");
        let id1 = project_profile_id(path);
        let id2 = project_profile_id(path);
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_different_projects_get_different_ids() {
        let id1 = project_profile_id(Path::new("/home/user/project-a"));
        let id2 = project_profile_id(Path::new("/home/user/project-b"));
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_profile_id_length() {
        let id = project_profile_id(Path::new("/some/path"));
        assert_eq!(id.len(), 16);
    }

    #[test]
    fn test_profile_path_includes_nix_profiles_dir() {
        // This test uses a relative path to avoid canonicalization issues
        let path = Path::new(".");
        let profile = profile_path_for_project(path).unwrap();
        assert!(profile.to_string_lossy().contains("nix-profiles"));
    }
}
