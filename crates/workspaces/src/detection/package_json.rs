use crate::core::types::PackageManager;
use crate::error::{Error, Result};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

use super::is_manager_detected;

pub(super) fn detect_manager_from_package_json(
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

fn has_js_manager(detected_managers: &HashSet<PackageManager>) -> bool {
    detected_managers.contains(&PackageManager::Npm)
        || detected_managers.contains(&PackageManager::Bun)
        || detected_managers.contains(&PackageManager::Pnpm)
        || detected_managers.contains(&PackageManager::YarnClassic)
        || detected_managers.contains(&PackageManager::YarnModern)
        || detected_managers.contains(&PackageManager::Deno)
}
