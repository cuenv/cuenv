//! Detect package managers from workspace files and command hints.

use super::scoring::{self, Detection, DetectionSignals};
use super::{command, filesystem, package_json};
use crate::core::types::PackageManager;
use crate::error::Result;
use std::collections::HashSet;
use std::path::Path;

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
/// ```rust
/// use cuenv_workspaces::{PackageManager, detection};
/// let root = tempfile::tempdir()?;
/// std::fs::write(root.path().join("package-lock.json"), "{}")?;
/// let managers = detection::detect_package_managers(root.path())?;
/// assert_eq!(managers[0], PackageManager::Npm);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// Returns an error if:
/// - Directory cannot be accessed
/// - Workspace config exists but is invalid
pub fn detect_package_managers(root: &Path) -> Result<Vec<PackageManager>> {
    tracing::debug!("Detecting package managers in: {}", root.display());

    let mut detections = Vec::new();
    let mut detected_managers = HashSet::new();

    detect_from_lockfiles(root, &mut detections, &mut detected_managers)?;
    detect_from_package_json(root, &mut detections, &mut detected_managers)?;
    detect_from_workspace_configs(root, &mut detections, &mut detected_managers)?;

    Ok(scoring::prioritize_managers(detections))
}

/// Detect package managers from lockfiles.
fn detect_from_lockfiles(
    root: &Path,
    detections: &mut Vec<Detection>,
    detected_managers: &mut HashSet<PackageManager>,
) -> Result<()> {
    let lockfiles = filesystem::find_lockfiles(root);
    tracing::debug!("Found {} lockfile(s)", lockfiles.len());

    for (manager, lockfile_path) in lockfiles {
        let detected = process_lockfile(root, manager, &lockfile_path)?;
        detections.push(detected);
        detected_managers.insert(detected.manager);
    }
    Ok(())
}

/// Process a single lockfile and return the detected manager with confidence.
fn process_lockfile(
    root: &Path,
    manager: PackageManager,
    lockfile_path: &Path,
) -> Result<Detection> {
    tracing::debug!(
        "Processing lockfile: {} ({})",
        lockfile_path.display(),
        manager
    );

    let detected_manager = if matches!(manager, PackageManager::YarnClassic) {
        filesystem::detect_yarn_version(lockfile_path)?
    } else {
        manager
    };

    let has_valid_config = filesystem::validate_workspace_config(root, detected_manager)?;
    let detection = Detection::from_signals(
        detected_manager,
        DetectionSignals {
            has_lockfile: true,
            has_valid_config,
        },
    );
    tracing::debug!(
        "Manager {} has confidence {}",
        detected_manager,
        detection.confidence
    );

    Ok(detection)
}

/// Detect package manager from package.json if not already detected.
fn detect_from_package_json(
    root: &Path,
    detections: &mut Vec<Detection>,
    detected_managers: &mut HashSet<PackageManager>,
) -> Result<()> {
    if let Some(manager) = package_json::detect_manager_from_package_json(root, detected_managers)?
    {
        let detection = Detection::from_signals(
            manager,
            DetectionSignals {
                has_lockfile: false,
                has_valid_config: true,
            },
        );
        tracing::debug!(
            "Manager {} detected via package.json (confidence {})",
            manager,
            detection.confidence
        );
        detections.push(detection);
        detected_managers.insert(manager);
    }
    Ok(())
}

/// Detect package managers from workspace configs without lockfiles.
fn detect_from_workspace_configs(
    root: &Path,
    detections: &mut Vec<Detection>,
    detected_managers: &mut HashSet<PackageManager>,
) -> Result<()> {
    for manager in [PackageManager::Pnpm, PackageManager::Cargo] {
        if is_manager_detected(detected_managers, manager) {
            continue;
        }

        if filesystem::validate_workspace_config(root, manager)? {
            let detection = Detection::from_signals(
                manager,
                DetectionSignals {
                    has_lockfile: false,
                    has_valid_config: true,
                },
            );
            tracing::debug!(
                "Manager {} detected via config only (confidence {})",
                manager,
                detection.confidence
            );
            detections.push(detection);
            detected_managers.insert(manager);
        }
    }
    Ok(())
}

/// Combines filesystem and command-based detection with command hint prioritization.
///
/// If a command hint is provided, the corresponding package manager will be
/// prioritized in the returned list if it was detected via filesystem scanning.
///
/// # Examples
///
/// ```rust
/// use cuenv_workspaces::{PackageManager, detection};
/// let root = tempfile::tempdir()?;
/// std::fs::write(root.path().join("Cargo.lock"), "")?;
/// std::fs::write(root.path().join("bun.lock"), "")?;
/// let managers = detection::detect_with_command_hint(root.path(), Some("bun run build"))?;
/// assert_eq!(managers[0], PackageManager::Bun);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// Returns an error if filesystem detection fails (e.g., unreadable directory).
pub fn detect_with_command_hint(root: &Path, command: Option<&str>) -> Result<Vec<PackageManager>> {
    let mut managers = detect_package_managers(root)?;

    // If we have a command hint, try to prioritize that manager
    if let Some(cmd) = command
        && let Some(hinted_manager) = command::detect_from_command(cmd)
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

pub(super) fn is_manager_detected(
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
