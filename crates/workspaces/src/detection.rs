//! Package manager detection module.
//!
//! This module provides functionality to detect which package managers are in use
//! within a workspace by scanning for lockfiles, workspace configurations, and
//! analyzing command strings.
//!
//! # Examples
//!
//! Detect package managers in a directory:
//!
//! ```no_run
//! use cuenv_workspaces::detection::detect_package_managers;
//! use std::path::Path;
//!
//! let root = Path::new("/path/to/workspace");
//! let managers = detect_package_managers(root)?;
//!
//! for manager in managers {
//!     println!("Detected: {}", manager);
//! }
//! # Ok::<(), cuenv_workspaces::Error>(())
//! ```
//!
//! Detect from a command string:
//!
//! ```
//! use cuenv_workspaces::detection::detect_from_command;
//! use cuenv_workspaces::PackageManager;
//!
//! assert_eq!(detect_from_command("cargo build"), Some(PackageManager::Cargo));
//! assert_eq!(detect_from_command("npm install"), Some(PackageManager::Npm));
//! assert_eq!(detect_from_command("bun run test"), Some(PackageManager::Bun));
//! ```

use crate::core::types::PackageManager;
use crate::error::{Error, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Detects all package managers present in the given directory.
///
/// This function scans for lockfiles and workspace configuration files,
/// then returns a list of detected package managers ordered by confidence
/// (highest first).
///
/// # Confidence scoring
///
/// - Lockfile + valid workspace config: 100
/// - Lockfile only: 75
/// - Valid workspace config only: 50
///
/// # Examples
///
/// ```no_run
/// use cuenv_workspaces::detection::detect_package_managers;
/// use std::path::Path;
///
/// let managers = detect_package_managers(Path::new("/workspace"))?;
/// if !managers.is_empty() {
///     println!("Primary package manager: {}", managers[0]);
/// }
/// # Ok::<(), cuenv_workspaces::Error>(())
/// ```
///
/// # Errors
///
/// Returns an error if:
/// - Directory cannot be accessed
/// - Workspace config exists but is invalid
pub fn detect_package_managers(root: &Path) -> Result<Vec<PackageManager>> {
    tracing::debug!("Detecting package managers in: {}", root.display());

    // Find all lockfiles present
    let lockfiles = find_lockfiles(root);
    tracing::debug!("Found {} lockfile(s)", lockfiles.len());

    // Build confidence scores for each detected manager
    let mut detections = Vec::new();
    let mut detected_managers = HashSet::new();

    for (manager, lockfile_path) in lockfiles {
        tracing::debug!(
            "Processing lockfile: {} ({})",
            lockfile_path.display(),
            manager
        );

        // Handle Yarn version detection specially
        let detected_manager = if matches!(manager, PackageManager::YarnClassic) {
            detect_yarn_version(&lockfile_path)?
        } else {
            manager
        };

        // Check if workspace config is valid
        let has_valid_config = validate_workspace_config(root, detected_manager)?;

        let confidence = calculate_confidence(true, has_valid_config);
        tracing::debug!("Manager {} has confidence {}", detected_manager, confidence);

        detections.push((detected_manager, confidence));
        detected_managers.insert(detected_manager);
    }

    if let Some(manager) = detect_manager_from_package_json(root, &detected_managers)? {
        let confidence = calculate_confidence(false, true);
        tracing::debug!(
            "Manager {} detected via package.json (confidence {})",
            manager,
            confidence
        );
        detections.push((manager, confidence));
        detected_managers.insert(manager);
    }

    // Also check for workspace configs without lockfiles for remaining managers
    for manager in [PackageManager::Pnpm, PackageManager::Cargo] {
        if is_manager_detected(&detected_managers, manager) {
            continue;
        }

        if validate_workspace_config(root, manager)? {
            let confidence = calculate_confidence(false, true);
            tracing::debug!(
                "Manager {} detected via config only (confidence {})",
                manager,
                confidence
            );
            detections.push((manager, confidence));
            detected_managers.insert(manager);
        }
    }

    Ok(prioritize_managers(detections))
}

/// Infers the package manager from a command string.
///
/// Maps common command names to their corresponding package managers.
///
/// # Examples
///
/// ```
/// use cuenv_workspaces::detection::detect_from_command;
/// use cuenv_workspaces::PackageManager;
///
/// assert_eq!(detect_from_command("cargo"), Some(PackageManager::Cargo));
/// assert_eq!(detect_from_command("npm"), Some(PackageManager::Npm));
/// assert_eq!(detect_from_command("bun"), Some(PackageManager::Bun));
/// assert_eq!(detect_from_command("pnpm"), Some(PackageManager::Pnpm));
/// assert_eq!(detect_from_command("node"), Some(PackageManager::Npm));
/// assert_eq!(detect_from_command("unknown"), None);
/// ```
pub fn detect_from_command(command: &str) -> Option<PackageManager> {
    // Extract the first word (command name)
    let cmd = command.split_whitespace().next().unwrap_or(command);

    match cmd {
        "cargo" => Some(PackageManager::Cargo),
        "npm" | "npx" | "node" => Some(PackageManager::Npm),
        "bun" | "bunx" => Some(PackageManager::Bun),
        "pnpm" => Some(PackageManager::Pnpm),
        "deno" => Some(PackageManager::Deno),
        "yarn" => {
            tracing::warn!(
                "'yarn' command detected; defaulting to YarnClassic. For accurate version detection, use lockfile analysis via detect_yarn_version()."
            );
            Some(PackageManager::YarnClassic)
        }
        _ => None,
    }
}

/// Combines filesystem and command-based detection with command hint prioritization.
///
/// If a command hint is provided, the corresponding package manager will be
/// prioritized in the returned list if it was detected via filesystem scanning.
///
/// # Examples
///
/// ```no_run
/// use cuenv_workspaces::detection::detect_with_command_hint;
/// use std::path::Path;
///
/// // Detect with hint that we're using Bun
/// let managers = detect_with_command_hint(
///     Path::new("/workspace"),
///     Some("bun run build")
/// )?;
///
/// // If both Cargo and Bun were detected, Bun will be first
/// # Ok::<(), cuenv_workspaces::Error>(())
/// ```
///
/// # Errors
///
/// Returns an error if filesystem detection fails (e.g., unreadable directory).
pub fn detect_with_command_hint(root: &Path, command: Option<&str>) -> Result<Vec<PackageManager>> {
    let mut managers = detect_package_managers(root)?;

    // If we have a command hint, try to prioritize that manager
    if let Some(cmd) = command
        && let Some(hinted_manager) = detect_from_command(cmd)
    {
        // Find the hinted manager in the list
        if let Some(pos) = managers.iter().position(|m| *m == hinted_manager) {
            // Move it to the front if it's not already there
            if pos > 0 {
                let manager = managers.remove(pos);
                managers.insert(0, manager);
                tracing::debug!("Prioritized {} based on command hint", manager);
            }
        }
    }

    Ok(managers)
}

/// Scans the root directory for package manager lockfiles.
///
/// Returns a list of tuples containing the detected package manager and the
/// path to its lockfile.
fn find_lockfiles(root: &Path) -> Vec<(PackageManager, PathBuf)> {
    let mut lockfiles = Vec::new();

    let candidates = [
        PackageManager::Npm,
        PackageManager::Bun,
        PackageManager::Pnpm,
        PackageManager::YarnClassic,
        PackageManager::Cargo,
        PackageManager::Deno,
    ];

    for manager in candidates {
        let lockfile_path = root.join(manager.lockfile_name());
        if lockfile_path.exists() {
            lockfiles.push((manager, lockfile_path));
        }
    }

    lockfiles
}

fn detect_manager_from_package_json(
    root: &Path,
    detected_managers: &HashSet<PackageManager>,
) -> Result<Option<PackageManager>> {
    let Some(package_json) = read_package_json(root)? else {
        return Ok(None);
    };

    let hinted_manager = package_json
        .get("packageManager")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_package_manager_hint);

    let manager = if let Some(manager) = hinted_manager {
        manager
    } else {
        if has_js_manager(detected_managers) {
            return Ok(None);
        }
        PackageManager::Npm
    };

    if is_manager_detected(detected_managers, manager) {
        return Ok(None);
    }

    Ok(Some(manager))
}

fn read_package_json(root: &Path) -> Result<Option<serde_json::Value>> {
    let path = root.join("package.json");
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path).map_err(|e| Error::Io {
        source: e,
        path: Some(path.clone()),
        operation: "reading workspace config".to_string(),
    })?;

    let parsed = serde_json::from_str::<serde_json::Value>(&content).map_err(|e| {
        Error::InvalidWorkspaceConfig {
            path: path.clone(),
            message: format!("Invalid JSON: {e}"),
        }
    })?;

    Ok(Some(parsed))
}

fn parse_package_manager_hint(hint: &str) -> Option<PackageManager> {
    let trimmed = hint.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (manager_name, version_part) = match trimmed.split_once('@') {
        Some((name, version)) if !name.is_empty() => (name, version),
        _ => (trimmed, ""),
    };

    let normalized_name = manager_name.trim().to_ascii_lowercase();

    match normalized_name.as_str() {
        "npm" => Some(PackageManager::Npm),
        "bun" => Some(PackageManager::Bun),
        "yarn" => {
            let major = parse_major_version(version_part);
            match major {
                Some(value) if value < 2 => Some(PackageManager::YarnClassic),
                _ => Some(PackageManager::YarnModern),
            }
        }
        _ => None,
    }
}

fn parse_major_version(input: &str) -> Option<u64> {
    let trimmed = input.trim().trim_start_matches(['v', 'V']);
    let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();

    if digits.is_empty() {
        return None;
    }

    digits.parse::<u64>().ok()
}

fn is_manager_detected(
    detected_managers: &HashSet<PackageManager>,
    manager: PackageManager,
) -> bool {
    match manager {
        PackageManager::YarnClassic | PackageManager::YarnModern => {
            detected_managers.contains(&PackageManager::YarnClassic)
                || detected_managers.contains(&PackageManager::YarnModern)
        }
        _ => detected_managers.contains(&manager),
    }
}

fn has_js_manager(detected_managers: &HashSet<PackageManager>) -> bool {
    detected_managers.contains(&PackageManager::Npm)
        || detected_managers.contains(&PackageManager::Bun)
        || detected_managers.contains(&PackageManager::Pnpm)
        || detected_managers.contains(&PackageManager::YarnClassic)
        || detected_managers.contains(&PackageManager::YarnModern)
        || detected_managers.contains(&PackageManager::Deno)
}

/// Validates that a workspace configuration file exists and is parseable.
///
/// Returns `Ok(true)` if the config is valid, `Ok(false)` if it doesn't exist,
/// or `Err` if it exists but is invalid.
fn validate_workspace_config(root: &Path, manager: PackageManager) -> Result<bool> {
    let config_path = root.join(manager.workspace_config_name());

    // Check if file exists
    if !config_path.exists() {
        return Ok(false);
    }

    // Read the file content
    let content = fs::read_to_string(&config_path).map_err(|e| Error::Io {
        source: e,
        path: Some(config_path.clone()),
        operation: "reading workspace config".to_string(),
    })?;

    // Try to parse based on file type
    match manager {
        PackageManager::Npm
        | PackageManager::Bun
        | PackageManager::YarnClassic
        | PackageManager::YarnModern
        | PackageManager::Deno => {
            // Parse as JSON (package.json or deno.json)
            serde_json::from_str::<serde_json::Value>(&content).map_err(|e| {
                Error::InvalidWorkspaceConfig {
                    path: config_path,
                    message: format!("Invalid JSON: {e}"),
                }
            })?;
            Ok(true)
        }
        PackageManager::Cargo => {
            // Parse as TOML (Cargo.toml)
            toml::from_str::<toml::Value>(&content).map_err(|e| Error::InvalidWorkspaceConfig {
                path: config_path,
                message: format!("Invalid TOML: {e}"),
            })?;
            Ok(true)
        }
        PackageManager::Pnpm => {
            // Parse as YAML (pnpm-workspace.yaml)
            serde_yaml::from_str::<serde_yaml::Value>(&content).map_err(|e| {
                Error::InvalidWorkspaceConfig {
                    path: config_path,
                    message: format!("Invalid YAML: {e}"),
                }
            })?;
            Ok(true)
        }
    }
}

/// Distinguishes between Yarn Classic (v1) and Yarn Modern (v2+).
///
/// Reads the lockfile format to determine the version:
/// - Yarn Classic uses a custom format starting with `# THIS IS AN AUTOGENERATED FILE`
/// - Yarn Modern (v2+) uses YAML format starting with `__metadata:`
fn detect_yarn_version(lockfile_path: &Path) -> Result<PackageManager> {
    let content = fs::read_to_string(lockfile_path).map_err(|e| Error::Io {
        source: e,
        path: Some(lockfile_path.to_path_buf()),
        operation: "reading yarn.lock".to_string(),
    })?;

    // Check the first few lines to determine version
    let first_lines: String = content.lines().take(5).collect::<Vec<_>>().join("\n");

    if first_lines.contains("__metadata:") {
        Ok(PackageManager::YarnModern)
    } else if first_lines.contains("# yarn lockfile v1")
        || first_lines.contains("# THIS IS AN AUTOGENERATED FILE")
    {
        Ok(PackageManager::YarnClassic)
    } else {
        // Default to Classic if we can't determine
        tracing::warn!(
            "Could not determine Yarn version from lockfile format, defaulting to Classic"
        );
        Ok(PackageManager::YarnClassic)
    }
}

/// Calculates a confidence score (0-100) based on detection signals.
///
/// Scoring:
/// - Lockfile + valid config: 100
/// - Lockfile only: 75
/// - Valid config only: 50
/// - Neither: 0
fn calculate_confidence(has_lockfile: bool, has_valid_config: bool) -> u8 {
    match (has_lockfile, has_valid_config) {
        (true, true) => 100,
        (true, false) => 75,
        (false, true) => 50,
        (false, false) => 0,
    }
}

/// Sorts detected package managers by confidence score and secondary ordering.
///
/// Primary sort: confidence score (descending)
/// Secondary sort: Cargo > Bun > pnpm > Yarn > npm
fn prioritize_managers(detections: Vec<(PackageManager, u8)>) -> Vec<PackageManager> {
    let mut sorted = detections;

    // Sort by confidence (descending), then by manager priority
    sorted.sort_by(|(m1, c1), (m2, c2)| {
        // First compare by confidence
        match c2.cmp(c1) {
            std::cmp::Ordering::Equal => {
                // If confidence is equal, use manager priority
                manager_priority(*m1).cmp(&manager_priority(*m2))
            }
            other => other,
        }
    });

    sorted.into_iter().map(|(m, _)| m).collect()
}

/// Returns a priority value for deterministic ordering when confidence is equal.
///
/// Lower values = higher priority
fn manager_priority(manager: PackageManager) -> u8 {
    match manager {
        PackageManager::Cargo => 0,
        PackageManager::Deno => 1,
        PackageManager::Bun => 2,
        PackageManager::Pnpm => 3,
        PackageManager::YarnModern => 4,
        PackageManager::YarnClassic => 5,
        PackageManager::Npm => 6,
    }
}

#[cfg(test)]
#[allow(clippy::match_same_arms)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Helper function to create a test workspace directory
    fn create_test_workspace() -> TempDir {
        TempDir::new().expect("Failed to create temp dir")
    }

    // Helper function to create a lockfile
    fn create_lockfile(dir: &Path, manager: PackageManager) -> PathBuf {
        let lockfile_path = dir.join(manager.lockfile_name());
        let content = match manager {
            PackageManager::Npm => r#"{"lockfileVersion": 2}"#,
            PackageManager::Bun => "binary content",
            PackageManager::Pnpm => "lockfileVersion: '6.0'",
            PackageManager::YarnClassic => "# yarn lockfile v1\n",
            PackageManager::YarnModern => "__metadata:\n  version: 6\n",
            PackageManager::Cargo => "[root]\n",
            PackageManager::Deno => r#"{"version": "3"}"#,
        };
        fs::write(&lockfile_path, content).expect("Failed to write lockfile");
        lockfile_path
    }

    // Helper function to create a workspace config
    fn create_workspace_config(dir: &Path, manager: PackageManager) -> PathBuf {
        let config_path = dir.join(manager.workspace_config_name());
        let content = match manager {
            PackageManager::Npm | PackageManager::Bun => {
                r#"{"name": "test", "workspaces": ["packages/*"]}"#
            }
            PackageManager::YarnClassic | PackageManager::YarnModern => {
                r#"{"name": "test", "workspaces": ["packages/*"]}"#
            }
            PackageManager::Pnpm => "packages:\n  - 'packages/*'\n",
            PackageManager::Cargo => "[workspace]\nmembers = [\"crates/*\"]\n",
            PackageManager::Deno => r#"{"name": "test", "workspace": ["packages/*"]}"#,
        };
        fs::write(&config_path, content).expect("Failed to write config");
        config_path
    }

    #[test]
    fn test_calculate_confidence() {
        assert_eq!(calculate_confidence(true, true), 100);
        assert_eq!(calculate_confidence(true, false), 75);
        assert_eq!(calculate_confidence(false, true), 50);
        assert_eq!(calculate_confidence(false, false), 0);
    }

    #[test]
    fn test_prioritize_managers() {
        let detections = vec![
            (PackageManager::Npm, 75),
            (PackageManager::Cargo, 100),
            (PackageManager::Bun, 75),
        ];

        let result = prioritize_managers(detections);

        assert_eq!(result[0], PackageManager::Cargo); // Highest confidence
        assert_eq!(result[1], PackageManager::Bun); // Same confidence as npm, but higher priority
        assert_eq!(result[2], PackageManager::Npm);
    }

    #[test]
    fn test_prioritize_managers_equal_confidence() {
        let detections = vec![
            (PackageManager::Npm, 75),
            (PackageManager::Bun, 75),
            (PackageManager::Cargo, 75),
        ];

        let result = prioritize_managers(detections);

        // With equal confidence, should be sorted by priority: Cargo > Bun > npm
        assert_eq!(result[0], PackageManager::Cargo);
        assert_eq!(result[1], PackageManager::Bun);
        assert_eq!(result[2], PackageManager::Npm);
    }

    #[test]
    fn test_detect_from_command() {
        assert_eq!(detect_from_command("cargo"), Some(PackageManager::Cargo));
        assert_eq!(detect_from_command("npm"), Some(PackageManager::Npm));
        assert_eq!(detect_from_command("npx"), Some(PackageManager::Npm));
        assert_eq!(detect_from_command("bun"), Some(PackageManager::Bun));
        assert_eq!(detect_from_command("bunx"), Some(PackageManager::Bun));
        assert_eq!(detect_from_command("pnpm"), Some(PackageManager::Pnpm));
        assert_eq!(detect_from_command("deno"), Some(PackageManager::Deno));
        assert_eq!(
            detect_from_command("yarn"),
            Some(PackageManager::YarnClassic)
        );
        assert_eq!(detect_from_command("node"), Some(PackageManager::Npm));
    }

    #[test]
    fn test_detect_from_command_with_args() {
        assert_eq!(
            detect_from_command("cargo build"),
            Some(PackageManager::Cargo)
        );
        assert_eq!(
            detect_from_command("npm install"),
            Some(PackageManager::Npm)
        );
        assert_eq!(
            detect_from_command("bun run test"),
            Some(PackageManager::Bun)
        );
        assert_eq!(
            detect_from_command("bunx eslint"),
            Some(PackageManager::Bun)
        );
        assert_eq!(
            detect_from_command("npx prisma generate"),
            Some(PackageManager::Npm)
        );
    }

    #[test]
    fn test_detect_from_command_unknown() {
        assert_eq!(detect_from_command("unknown"), None);
        assert_eq!(detect_from_command("make"), None);
        assert_eq!(detect_from_command("python"), None);
    }

    #[test]
    fn test_detect_yarn_classic() {
        let temp_dir = create_test_workspace();
        let lockfile_path = temp_dir.path().join("yarn.lock");

        let classic_content = r#"# yarn lockfile v1
# THIS IS AN AUTOGENERATED FILE. DO NOT EDIT THIS FILE DIRECTLY.

package@^1.0.0:
  version "1.0.0"
"#;
        fs::write(&lockfile_path, classic_content).unwrap();

        let result = detect_yarn_version(&lockfile_path).unwrap();
        assert_eq!(result, PackageManager::YarnClassic);
    }

    #[test]
    fn test_detect_yarn_modern() {
        let temp_dir = create_test_workspace();
        let lockfile_path = temp_dir.path().join("yarn.lock");

        let modern_content = r#"__metadata:
  version: 6
  cacheKey: 8

"package@npm:^1.0.0":
  version: 1.0.0
"#;
        fs::write(&lockfile_path, modern_content).unwrap();

        let result = detect_yarn_version(&lockfile_path).unwrap();
        assert_eq!(result, PackageManager::YarnModern);
    }

    #[test]
    fn test_find_lockfiles_cargo() {
        let temp_dir = create_test_workspace();
        create_lockfile(temp_dir.path(), PackageManager::Cargo);

        let result = find_lockfiles(temp_dir.path());

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, PackageManager::Cargo);
    }

    #[test]
    fn test_find_lockfiles_npm() {
        let temp_dir = create_test_workspace();
        create_lockfile(temp_dir.path(), PackageManager::Npm);

        let result = find_lockfiles(temp_dir.path());

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, PackageManager::Npm);
    }

    #[test]
    fn test_find_lockfiles_multiple() {
        let temp_dir = create_test_workspace();
        create_lockfile(temp_dir.path(), PackageManager::Cargo);
        create_lockfile(temp_dir.path(), PackageManager::Npm);

        let result = find_lockfiles(temp_dir.path());

        assert_eq!(result.len(), 2);
        let managers: Vec<_> = result.iter().map(|(m, _)| *m).collect();
        assert!(managers.contains(&PackageManager::Cargo));
        assert!(managers.contains(&PackageManager::Npm));
    }

    #[test]
    fn test_find_lockfiles_none() {
        let temp_dir = create_test_workspace();

        let result = find_lockfiles(temp_dir.path());

        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_find_lockfiles_bun_text() {
        let temp_dir = create_test_workspace();

        fs::write(temp_dir.path().join("bun.lock"), "text").unwrap();

        let result = find_lockfiles(temp_dir.path());

        // Should detect the text bun.lock
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, PackageManager::Bun);
        assert_eq!(result[0].1.file_name().unwrap(), "bun.lock");
    }

    #[test]
    fn test_validate_workspace_config_cargo() {
        let temp_dir = create_test_workspace();
        create_workspace_config(temp_dir.path(), PackageManager::Cargo);

        let result = validate_workspace_config(temp_dir.path(), PackageManager::Cargo).unwrap();
        assert!(result);
    }

    #[test]
    fn test_validate_workspace_config_package_json() {
        let temp_dir = create_test_workspace();
        create_workspace_config(temp_dir.path(), PackageManager::Npm);

        let result = validate_workspace_config(temp_dir.path(), PackageManager::Npm).unwrap();
        assert!(result);
    }

    #[test]
    fn test_validate_workspace_config_pnpm() {
        let temp_dir = create_test_workspace();
        create_workspace_config(temp_dir.path(), PackageManager::Pnpm);

        let result = validate_workspace_config(temp_dir.path(), PackageManager::Pnpm).unwrap();
        assert!(result);
    }

    #[test]
    fn test_validate_workspace_config_invalid_json() {
        let temp_dir = create_test_workspace();
        let config_path = temp_dir.path().join("package.json");
        fs::write(&config_path, "{ invalid json }").unwrap();

        let result = validate_workspace_config(temp_dir.path(), PackageManager::Npm);
        assert!(result.is_err());
        assert!(matches!(result, Err(Error::InvalidWorkspaceConfig { .. })));
    }

    #[test]
    fn test_validate_workspace_config_invalid_toml() {
        let temp_dir = create_test_workspace();
        let config_path = temp_dir.path().join("Cargo.toml");
        fs::write(&config_path, "[invalid toml").unwrap();

        let result = validate_workspace_config(temp_dir.path(), PackageManager::Cargo);
        assert!(result.is_err());
        assert!(matches!(result, Err(Error::InvalidWorkspaceConfig { .. })));
    }

    #[test]
    fn test_validate_workspace_config_invalid_yaml() {
        let temp_dir = create_test_workspace();
        let config_path = temp_dir.path().join("pnpm-workspace.yaml");
        fs::write(&config_path, "invalid: yaml: content:").unwrap();

        let result = validate_workspace_config(temp_dir.path(), PackageManager::Pnpm);
        assert!(result.is_err());
        assert!(matches!(result, Err(Error::InvalidWorkspaceConfig { .. })));
    }

    #[test]
    fn test_validate_workspace_config_missing() {
        let temp_dir = create_test_workspace();

        let result = validate_workspace_config(temp_dir.path(), PackageManager::Npm).unwrap();
        assert!(!result); // Should return false, not error
    }

    #[test]
    fn test_detect_package_managers_cargo() {
        let temp_dir = create_test_workspace();
        create_lockfile(temp_dir.path(), PackageManager::Cargo);
        create_workspace_config(temp_dir.path(), PackageManager::Cargo);

        let result = detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], PackageManager::Cargo);
    }

    #[test]
    fn test_detect_package_managers_npm() {
        let temp_dir = create_test_workspace();
        create_lockfile(temp_dir.path(), PackageManager::Npm);
        create_workspace_config(temp_dir.path(), PackageManager::Npm);

        let result = detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result, vec![PackageManager::Npm]);
    }

    #[test]
    fn test_detect_package_managers_multi() {
        let temp_dir = create_test_workspace();

        // Create both Cargo and Bun files
        create_lockfile(temp_dir.path(), PackageManager::Cargo);
        create_workspace_config(temp_dir.path(), PackageManager::Cargo);

        // For Bun, we need to create package.json (same as npm config)
        fs::write(
            temp_dir.path().join("package.json"),
            r#"{"name": "test", "workspaces": ["packages/*"]}"#,
        )
        .unwrap();
        create_lockfile(temp_dir.path(), PackageManager::Bun);

        let result = detect_package_managers(temp_dir.path()).unwrap();

        // Cargo (lockfile + config = 100) and Bun (lockfile + config = 100) are detected
        // We should have at least 2 with Cargo and Bun first (sorted by priority)
        assert!(result.len() >= 2);
        // Cargo and Bun both have confidence 100, sorted by priority: Cargo > Bun
        assert_eq!(result[0], PackageManager::Cargo);
        assert_eq!(result[1], PackageManager::Bun);
    }

    #[test]
    fn test_detect_package_managers_lockfile_only() {
        let temp_dir = create_test_workspace();
        create_lockfile(temp_dir.path(), PackageManager::Cargo);
        // No workspace config

        let result = detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], PackageManager::Cargo);
    }

    #[test]
    fn test_detect_package_managers_config_only() {
        let temp_dir = create_test_workspace();
        create_workspace_config(temp_dir.path(), PackageManager::Cargo);
        // No lockfile

        let result = detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], PackageManager::Cargo);
    }

    #[test]
    fn test_detect_package_managers_empty_dir() {
        let temp_dir = create_test_workspace();

        let result = detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_detect_with_command_hint() {
        let temp_dir = create_test_workspace();

        // Create both Cargo and Bun files
        create_lockfile(temp_dir.path(), PackageManager::Cargo);
        create_workspace_config(temp_dir.path(), PackageManager::Cargo);

        fs::write(
            temp_dir.path().join("package.json"),
            r#"{"name": "test", "workspaces": ["packages/*"]}"#,
        )
        .unwrap();
        create_lockfile(temp_dir.path(), PackageManager::Bun);

        // Without hint, Cargo comes first (higher priority)
        let result = detect_package_managers(temp_dir.path()).unwrap();
        assert_eq!(result[0], PackageManager::Cargo);

        // With Bun hint, Bun should come first
        let result = detect_with_command_hint(temp_dir.path(), Some("bun run test")).unwrap();
        assert_eq!(result[0], PackageManager::Bun);
        assert_eq!(result[1], PackageManager::Cargo);
    }

    #[test]
    fn test_detect_with_command_hint_not_detected() {
        let temp_dir = create_test_workspace();
        create_lockfile(temp_dir.path(), PackageManager::Cargo);
        create_workspace_config(temp_dir.path(), PackageManager::Cargo);

        // Hint for a manager that's not detected - should not affect results
        let result = detect_with_command_hint(temp_dir.path(), Some("npm install")).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], PackageManager::Cargo);
    }

    #[test]
    fn test_detect_with_no_command_hint() {
        let temp_dir = create_test_workspace();
        create_lockfile(temp_dir.path(), PackageManager::Cargo);

        let result = detect_with_command_hint(temp_dir.path(), None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], PackageManager::Cargo);
    }

    #[test]
    fn test_yarn_version_detection_in_detect_package_managers() {
        let temp_dir = create_test_workspace();

        // Create a Yarn Modern lockfile
        let lockfile_path = temp_dir.path().join("yarn.lock");
        fs::write(&lockfile_path, "__metadata:\n  version: 6\n").unwrap();
        create_workspace_config(temp_dir.path(), PackageManager::YarnModern);

        let result = detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result, vec![PackageManager::YarnModern]);
    }

    #[test]
    fn test_package_json_config_only_defaults_to_npm() {
        let temp_dir = create_test_workspace();

        fs::write(
            temp_dir.path().join("package.json"),
            r#"{"name":"example"}"#,
        )
        .unwrap();

        let result = detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result, vec![PackageManager::Npm]);
    }

    #[test]
    fn test_package_json_package_manager_hint_yarn_classic() {
        let temp_dir = create_test_workspace();

        fs::write(
            temp_dir.path().join("package.json"),
            r#"{"name":"example","packageManager":"yarn@1.22.0"}"#,
        )
        .unwrap();

        let result = detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result, vec![PackageManager::YarnClassic]);
    }

    #[test]
    fn test_package_json_package_manager_hint_yarn_modern() {
        let temp_dir = create_test_workspace();

        fs::write(
            temp_dir.path().join("package.json"),
            r#"{"name":"example","packageManager":"yarn@3.5.1"}"#,
        )
        .unwrap();

        let result = detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result, vec![PackageManager::YarnModern]);
    }

    #[test]
    fn test_package_json_package_manager_hint_bun() {
        let temp_dir = create_test_workspace();

        fs::write(
            temp_dir.path().join("package.json"),
            r#"{"name":"example","packageManager":"bun@1.0.0"}"#,
        )
        .unwrap();

        let result = detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result, vec![PackageManager::Bun]);
    }

    #[test]
    fn test_yarn_modern_lockfile_does_not_duplicate_classic() {
        let temp_dir = create_test_workspace();
        create_lockfile(temp_dir.path(), PackageManager::YarnModern);
        fs::write(
            temp_dir.path().join("package.json"),
            r#"{"name":"example"}"#,
        )
        .unwrap();

        let result = detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result, vec![PackageManager::YarnModern]);
    }

    #[test]
    fn test_manager_priority() {
        assert!(manager_priority(PackageManager::Cargo) < manager_priority(PackageManager::Deno));
        assert!(manager_priority(PackageManager::Deno) < manager_priority(PackageManager::Bun));
        assert!(manager_priority(PackageManager::Bun) < manager_priority(PackageManager::Pnpm));
        assert!(
            manager_priority(PackageManager::Pnpm) < manager_priority(PackageManager::YarnModern)
        );
        assert!(
            manager_priority(PackageManager::YarnModern)
                < manager_priority(PackageManager::YarnClassic)
        );
        assert!(
            manager_priority(PackageManager::YarnClassic) < manager_priority(PackageManager::Npm)
        );
    }
}
