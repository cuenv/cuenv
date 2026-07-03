//! Tool activation support for CI task execution.

use super::ExecutorError;
use cuenv_core::lockfile::{LOCKFILE_NAME, Lockfile};
use cuenv_core::tools::{
    Platform, ResolvedTool, ResolvedToolActivationStep, ToolActivationResolveOptions, ToolOptions,
    ToolRegistry, apply_resolved_tool_activation, resolve_tool_activation,
    validate_tool_activation,
};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Find the lockfile starting from a directory.
fn find_lockfile(start_dir: &Path) -> Option<PathBuf> {
    let lockfile_path = start_dir.join(LOCKFILE_NAME);
    if lockfile_path.exists() {
        return Some(lockfile_path);
    }

    let mut current = start_dir.parent();
    while let Some(dir) = current {
        let lockfile_path = dir.join(LOCKFILE_NAME);
        if lockfile_path.exists() {
            return Some(lockfile_path);
        }
        current = dir.parent();
    }

    None
}

/// Create a tool registry with all available providers.
fn create_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    registry.register(cuenv_tools_nix::NixToolProvider::new());
    registry.register(cuenv_tools_github::GitHubToolProvider::new());
    registry.register(cuenv_tools_rustup::RustupToolProvider::new());
    registry.register(cuenv_tools_url::UrlToolProvider::new());

    registry
}

/// Resolve activation steps from lockfile for CI execution.
pub(super) fn resolve_tool_activation_steps(
    project_root: &Path,
) -> std::result::Result<Vec<ResolvedToolActivationStep>, ExecutorError> {
    let Some(lockfile_path) = find_lockfile(project_root) else {
        return Ok(Vec::new());
    };

    let lockfile = match Lockfile::load(&lockfile_path) {
        Ok(Some(lf)) => lf,
        Ok(None) => return Ok(Vec::new()),
        Err(e) => {
            return Err(ExecutorError::Compilation(format!(
                "Failed to load lockfile: {e}"
            )));
        }
    };

    let options = ToolActivationResolveOptions::new(&lockfile, &lockfile_path);
    resolve_tool_activation(&options).map_err(|e| {
        ExecutorError::Compilation(format!("Invalid tool activation configuration: {e}"))
    })
}

pub(super) fn apply_tool_activation_steps(
    env: &mut BTreeMap<String, String>,
    steps: &[ResolvedToolActivationStep],
) {
    for step in steps {
        let current = env.get(&step.var).map(String::as_str);
        if let Some(new_value) = apply_resolved_tool_activation(current, step) {
            env.insert(step.var.clone(), new_value);
        }
    }
}

/// Ensure all tools from the lockfile are downloaded for the current platform.
pub(super) async fn ensure_tools_downloaded(
    project_root: &Path,
) -> std::result::Result<(), ExecutorError> {
    let Some(lockfile_path) = find_lockfile(project_root) else {
        tracing::debug!("No lockfile found - skipping tool download");
        return Ok(());
    };

    let lockfile = match Lockfile::load(&lockfile_path) {
        Ok(Some(lf)) => lf,
        Ok(None) => {
            tracing::debug!("Empty lockfile - skipping tool download");
            return Ok(());
        }
        Err(e) => {
            return Err(ExecutorError::Compilation(format!(
                "Failed to load lockfile: {e}"
            )));
        }
    };

    if lockfile.tools.is_empty() {
        tracing::debug!("No tools in lockfile - skipping download");
        return Ok(());
    }

    let activation_options = ToolActivationResolveOptions::new(&lockfile, &lockfile_path);
    validate_tool_activation(&activation_options).map_err(|e| {
        ExecutorError::Compilation(format!("Invalid tool activation configuration: {e}"))
    })?;

    let platform = Platform::current();
    let platform_str = platform.to_string();
    let options = ToolOptions::default();
    let registry = create_tool_registry();

    let mut providers_used = HashSet::new();
    for tool in lockfile.tools.values() {
        if let Some(locked) = tool.platforms.get(&platform_str) {
            providers_used.insert(locked.provider.clone());
        }
    }

    for provider_name in &providers_used {
        if let Some(provider) = registry.get(provider_name)
            && let Err(e) = provider.check_prerequisites().await
        {
            tracing::warn!(
                "Provider '{}' prerequisites check failed: {} - skipping tools from this provider",
                provider_name,
                e
            );
        }
    }

    let mut errors: Vec<String> = Vec::new();

    for (name, tool) in &lockfile.tools {
        let Some(locked) = tool.platforms.get(&platform_str) else {
            continue;
        };

        let Some(source) = locked.to_tool_source() else {
            tracing::debug!(
                "Unknown provider '{}' for tool '{}' - skipping",
                locked.provider,
                name
            );
            continue;
        };

        let Some(provider) = registry.find_for_source(&source) else {
            tracing::debug!("No provider found for tool '{}' - skipping", name);
            continue;
        };

        let resolved = ResolvedTool {
            name: name.clone(),
            version: tool.version.clone(),
            platform: platform.clone(),
            source,
        };

        if provider.is_cached(&resolved, &options) {
            continue;
        }

        tracing::info!("Downloading {} v{}...", name, tool.version);
        match provider.fetch(&resolved, &options).await {
            Ok(fetched) => {
                tracing::info!("Downloaded {} -> {}", name, fetched.binary_path.display());
            }
            Err(e) => {
                tracing::warn!("Failed to download tool '{}': {}", name, e);
                errors.push(format!("{}: {}", name, e));
            }
        }
    }

    if !errors.is_empty() {
        return Err(ExecutorError::Compilation(format!(
            "Failed to download tools: {}",
            errors.join(", ")
        )));
    }

    Ok(())
}
