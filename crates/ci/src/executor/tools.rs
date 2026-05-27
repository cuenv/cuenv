//! Tool activation support for CI task execution.

use super::ExecutorError;
use cuenv_core::lockfile::{LOCKFILE_NAME, LockedToolPlatform, Lockfile};
use cuenv_core::tools::{
    Platform, ResolvedTool, ResolvedToolActivationStep, ToolActivationResolveOptions, ToolExtract,
    ToolOptions, ToolRegistry, ToolSource, apply_resolved_tool_activation, resolve_tool_activation,
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

/// Convert a lockfile entry to a `ToolSource`.
fn lockfile_entry_to_source(locked: &LockedToolPlatform) -> Option<ToolSource> {
    match locked.provider.as_str() {
        "oci" => {
            let image = locked
                .source
                .get("image")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let path = locked
                .source
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            Some(ToolSource::Oci {
                image: image.to_string(),
                path: path.to_string(),
            })
        }
        "github" => {
            let repo = locked
                .source
                .get("repo")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let tag = locked
                .source
                .get("tag")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let asset = locked
                .source
                .get("asset")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let extract = parse_github_extract_list(&locked.source);
            Some(ToolSource::GitHub {
                repo: repo.to_string(),
                tag: tag.to_string(),
                asset: asset.to_string(),
                extract,
            })
        }
        "nix" => {
            let flake = locked
                .source
                .get("flake")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let package = locked
                .source
                .get("package")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let output = locked
                .source
                .get("output")
                .and_then(|v| v.as_str())
                .map(String::from);
            Some(ToolSource::Nix {
                flake: flake.to_string(),
                package: package.to_string(),
                output,
            })
        }
        "rustup" => {
            let toolchain = locked
                .source
                .get("toolchain")
                .and_then(|v| v.as_str())
                .unwrap_or("stable");
            let profile = locked
                .source
                .get("profile")
                .and_then(|v| v.as_str())
                .map(String::from);
            let components: Vec<String> = locked
                .source
                .get("components")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let targets: Vec<String> = locked
                .source
                .get("targets")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            Some(ToolSource::Rustup {
                toolchain: toolchain.to_string(),
                profile,
                components,
                targets,
            })
        }
        "url" => {
            let url = locked
                .source
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let extract = parse_github_extract_list(&locked.source);
            Some(ToolSource::Url {
                url: url.to_string(),
                extract,
            })
        }
        _ => None,
    }
}

fn parse_github_extract_list(source: &serde_json::Value) -> Vec<ToolExtract> {
    let mut extract = source
        .get("extract")
        .cloned()
        .and_then(|value| serde_json::from_value::<Vec<ToolExtract>>(value).ok())
        .unwrap_or_default();

    if extract.is_empty()
        && let Some(path) = source.get("path").and_then(|v| v.as_str())
    {
        if path_looks_like_library(path) {
            extract.push(ToolExtract::Lib {
                path: path.to_string(),
                env: None,
            });
        } else {
            extract.push(ToolExtract::Bin {
                path: path.to_string(),
                as_name: None,
            });
        }
    }

    extract
}

fn path_looks_like_library(path: &str) -> bool {
    let ext_is = |target: &str| {
        std::path::Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case(target))
    };
    ext_is("dylib") || ext_is("so") || path.to_ascii_lowercase().contains(".so.") || ext_is("dll")
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

        let Some(source) = lockfile_entry_to_source(locked) else {
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
