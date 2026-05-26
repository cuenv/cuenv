use cuenv_workspaces::PackageManager;
use std::path::{Component, Path, PathBuf};

pub(super) fn find_workspace_root(manager: PackageManager, start: &Path) -> PathBuf {
    let mut current = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());

    loop {
        let is_root = match manager {
            PackageManager::Npm
            | PackageManager::Bun
            | PackageManager::YarnClassic
            | PackageManager::YarnModern => package_json_has_workspaces(&current),
            PackageManager::Pnpm => current.join("pnpm-workspace.yaml").exists(),
            PackageManager::Cargo => cargo_toml_has_workspace(&current),
            PackageManager::Deno => deno_json_has_workspace(&current),
        };

        if is_root {
            return current;
        }

        if let Some(parent) = current.parent() {
            current = parent.to_path_buf();
        } else {
            return start.to_path_buf();
        }
    }
}

pub(super) fn normalize_join(base: PathBuf, path: &str) -> PathBuf {
    let candidate = base.join(path);
    let mut normalized = PathBuf::new();

    for component in candidate.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }

    normalized
}

pub(super) fn package_json_has_workspaces(dir: &Path) -> bool {
    let path = dir.join("package.json");
    let content = std::fs::read_to_string(&path);
    let Ok(json) = content.and_then(|s| {
        serde_json::from_str::<serde_json::Value>(&s)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }) else {
        return false;
    };

    match json.get("workspaces") {
        Some(serde_json::Value::Array(arr)) => !arr.is_empty(),
        Some(serde_json::Value::Object(map)) => map
            .get("packages")
            .and_then(|packages| packages.as_array())
            .is_some_and(|arr| !arr.is_empty()),
        _ => false,
    }
}

pub(super) fn cargo_toml_has_workspace(dir: &Path) -> bool {
    let path = dir.join("Cargo.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };

    content.contains("[workspace]")
}

pub(super) fn deno_json_has_workspace(dir: &Path) -> bool {
    let path = dir.join("deno.json");
    let content = std::fs::read_to_string(&path);
    let Ok(json) = content.and_then(|s| {
        serde_json::from_str::<serde_json::Value>(&s)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }) else {
        return false;
    };

    match json.get("workspace") {
        Some(serde_json::Value::Array(arr)) => !arr.is_empty(),
        Some(serde_json::Value::Object(_)) => true,
        _ => false,
    }
}
