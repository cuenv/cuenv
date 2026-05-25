//! Tools command implementations for multi-source tool management.
//!
//! This module provides commands for downloading, activating, and listing tools
//! from multiple sources (GitHub releases, Nix packages, OCI images).

use crate::cli::CliError;
use cuenv_core::lockfile::{LOCKFILE_NAME, Lockfile};
use cuenv_core::tools::{
    Platform, ResolvedToolActivationStep, ToolActivationResolveOptions, ToolExtract, ToolOptions,
    ToolRegistry, ToolSource, apply_resolved_tool_activation, resolve_tool_activation,
    validate_tool_activation,
};
use cuenv_events::{eprintln_redacted, println_redacted};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

mod list;
use list::render_tools_list;

/// Create a tool registry with available providers.
fn create_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    // Register Nix provider
    registry.register(cuenv_tools_nix::NixToolProvider::new());

    // Register GitHub provider
    registry.register(cuenv_tools_github::GitHubToolProvider::new());

    // Register Rustup provider
    registry.register(cuenv_tools_rustup::RustupToolProvider::new());

    // Register URL provider
    registry.register(cuenv_tools_url::UrlToolProvider::new());

    // Register OCI provider
    registry.register(cuenv_tools_oci::OciToolProvider::new());

    registry
}

/// Execute the `tools download` command.
///
/// Downloads tools for the current platform from the lockfile.
///
/// # Errors
///
/// Returns an error if the lockfile is not found or if any tool download fails.
pub async fn execute_tools_download() -> Result<(), CliError> {
    // Find the lockfile
    let lockfile_path = find_lockfile(None).ok_or_else(|| {
        CliError::config_with_help(
            "No cuenv.lock found",
            "Run 'cuenv sync lock' to create the lockfile",
        )
    })?;

    // Load the lockfile
    let lockfile = Lockfile::load(&lockfile_path)
        .map_err(|e| CliError::other(format!("Failed to load lockfile: {e}")))?
        .ok_or_else(|| {
            CliError::config_with_help(
                "Lockfile is empty",
                "Run 'cuenv sync lock' to populate the lockfile",
            )
        })?;

    // Get current platform
    let platform = Platform::current();
    let platform_str = platform.to_string();

    // Create tool options
    let options = ToolOptions::default();

    // Create the registry
    let registry = create_registry();

    // Check prerequisites for all providers we'll use
    let mut providers_used = HashSet::new();
    for tool in lockfile.tools.values() {
        if let Some(locked) = tool.platforms.get(&platform_str) {
            providers_used.insert(locked.provider.clone());
        }
    }

    for provider_name in &providers_used {
        if let Some(provider) = registry.get(provider_name) {
            provider.check_prerequisites().await.map_err(|e| {
                CliError::config_with_help(
                    format!("Provider '{}' not available: {}", provider_name, e),
                    "Check that the required tools are installed",
                )
            })?;
        }
    }

    // Download tools
    let mut downloaded = 0;
    let mut skipped = 0;
    let mut errors: Vec<String> = Vec::new();

    for (name, tool) in &lockfile.tools {
        let Some(locked) = tool.platforms.get(&platform_str) else {
            // Tool not available for this platform
            continue;
        };

        let Some(source) = lockfile_entry_to_source(name, &tool.version, locked) else {
            eprintln_redacted(&format!(
                "Warning: Unknown provider '{}' for tool '{}'",
                locked.provider, name
            ));
            continue;
        };

        // Get the provider
        let Some(provider) = registry.find_for_source(&source) else {
            eprintln_redacted(&format!(
                "Warning: No provider found for source type of tool '{}'",
                name
            ));
            continue;
        };

        // Create resolved tool
        let resolved = cuenv_core::tools::ResolvedTool {
            name: name.clone(),
            version: tool.version.clone(),
            platform: platform.clone(),
            source,
        };

        // Check if already cached
        if provider.is_cached(&resolved, &options) {
            skipped += 1;
            continue;
        }

        // Fetch the tool
        println_redacted(&format!("Downloading {} v{}...", name, tool.version));
        match provider.fetch(&resolved, &options).await {
            Ok(fetched) => {
                println_redacted(&format!(
                    "  -> {} ({})",
                    fetched.binary_path.display(),
                    fetched.sha256
                ));
                downloaded += 1;
            }
            Err(e) => {
                eprintln_redacted(&format!("  Error downloading '{}': {}", name, e));
                errors.push(format!("{}: {}", name, e));
            }
        }
    }

    println_redacted("");
    println_redacted(&format!(
        "Downloaded {} tools, {} already cached",
        downloaded, skipped
    ));

    if !errors.is_empty() {
        return Err(CliError::other(format!(
            "Failed to download tools: {}",
            errors.join(", ")
        )));
    }

    Ok(())
}

/// Ensure all tools from the lockfile are downloaded for the current platform.
///
/// This is called automatically before tool activation in exec/task commands.
/// If no lockfile exists or tools are already cached, this is a no-op.
///
/// If `project_path` is provided, the lockfile search starts from that directory.
/// Otherwise, it starts from the current working directory.
///
/// # Errors
///
/// Returns an error if tools cannot be downloaded due to provider issues.
pub async fn ensure_tools_downloaded(project_path: Option<&Path>) -> Result<(), CliError> {
    // Find the lockfile - not finding one is not an error
    let Some(lockfile_path) = find_runtime_lockfile(project_path) else {
        tracing::debug!("No lockfile found - skipping tool download");
        return Ok(());
    };

    // Load the lockfile
    let Some(lockfile) = Lockfile::load(&lockfile_path)
        .map_err(|e| CliError::other(format!("Failed to load lockfile: {e}")))?
    else {
        tracing::debug!("Empty lockfile - skipping tool download");
        return Ok(());
    };

    if lockfile.tools.is_empty() {
        tracing::debug!("No tools in lockfile - skipping download");
        return Ok(());
    }

    // Validate lockfile activation hints before any download activity.
    let activation_options = ToolActivationResolveOptions::new(&lockfile, &lockfile_path);
    validate_tool_activation(&activation_options).map_err(|e| {
        CliError::config_with_help(
            format!("Invalid tool activation configuration: {e}"),
            "Run 'cuenv sync lock' to refresh cuenv.lock",
        )
    })?;

    // Get current platform
    let platform = Platform::current();
    let platform_str = platform.to_string();

    // Create tool options
    let options = ToolOptions::default();

    // Create the registry
    let registry = create_registry();

    // Check prerequisites for all providers we'll use
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

    // Download tools that aren't cached
    let mut downloaded = 0;
    let mut errors: Vec<String> = Vec::new();

    for (name, tool) in &lockfile.tools {
        let Some(locked) = tool.platforms.get(&platform_str) else {
            // Tool not available for this platform
            continue;
        };

        // Convert lockfile data to ToolSource
        let Some(source) = lockfile_entry_to_source(name, &tool.version, locked) else {
            tracing::debug!(
                "Unknown provider '{}' for tool '{}' - skipping",
                locked.provider,
                name
            );
            continue;
        };

        // Get the provider
        let Some(provider) = registry.find_for_source(&source) else {
            tracing::debug!("No provider found for tool '{}' - skipping", name);
            continue;
        };

        // Create resolved tool
        let resolved = cuenv_core::tools::ResolvedTool {
            name: name.clone(),
            version: tool.version.clone(),
            platform: platform.clone(),
            source,
        };

        // Check if already cached
        if provider.is_cached(&resolved, &options) {
            continue;
        }

        // Fetch the tool
        tracing::info!("Downloading {} v{}...", name, tool.version);
        match provider.fetch(&resolved, &options).await {
            Ok(fetched) => {
                tracing::info!(
                    "Downloaded {} -> {} ({})",
                    name,
                    fetched.binary_path.display(),
                    fetched.sha256
                );
                downloaded += 1;
            }
            Err(e) => {
                tracing::warn!("Failed to download '{}': {}", name, e);
                errors.push(format!("{}: {}", name, e));
            }
        }
    }

    if downloaded > 0 {
        tracing::info!("Downloaded {} tools", downloaded);
    }

    if !errors.is_empty() {
        return Err(CliError::other(format!(
            "Failed to download tools: {}",
            errors.join(", ")
        )));
    }

    Ok(())
}

/// Convert a lockfile entry to a ToolSource.
fn lockfile_entry_to_source(
    _name: &str,
    _version: &str,
    locked: &cuenv_core::lockfile::LockedToolPlatform,
) -> Option<ToolSource> {
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
            let components = locked
                .source
                .get("components")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let targets = locked
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

/// Resolve inferred activation steps from the lockfile for the current platform.
///
/// Returns `Ok(None)` when no lockfile exists or no activation steps resolve.
///
/// # Errors
///
/// Returns an error when lockfile parsing fails or activation hints are invalid.
pub fn resolve_tool_activation_steps(
    project_path: Option<&Path>,
) -> Result<Option<Vec<ResolvedToolActivationStep>>, CliError> {
    let Some(lockfile_path) = find_runtime_lockfile(project_path) else {
        return Ok(None);
    };

    let Some(lockfile) = Lockfile::load(&lockfile_path)
        .map_err(|e| CliError::other(format!("Failed to load lockfile: {e}")))?
    else {
        return Ok(None);
    };

    let options = ToolActivationResolveOptions::new(&lockfile, &lockfile_path);
    let activation = resolve_tool_activation(&options).map_err(|e| {
        CliError::config_with_help(
            format!("Invalid tool activation configuration: {e}"),
            "Run 'cuenv sync lock' to refresh cuenv.lock",
        )
    })?;

    if activation.is_empty() {
        return Ok(None);
    }

    Ok(Some(activation))
}

/// Execute the `tools activate` command.
///
/// Outputs shell export statements inferred from lockfile tool metadata.
///
/// # Errors
///
/// Returns an error if the lockfile is not found.
pub fn execute_tools_activate() -> Result<(), CliError> {
    let activation_steps = resolve_tool_activation_steps(None)?.ok_or_else(|| {
        CliError::config_with_help(
            "No cuenv.lock found or no tools configured",
            "Run 'cuenv sync lock' to create the lockfile",
        )
    })?;

    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    let mut touched_vars: Vec<String> = Vec::new();
    let mut touched_set: HashSet<String> = HashSet::new();

    for step in activation_steps {
        let current = env.get(&step.var).map(String::as_str);
        if let Some(new_value) = apply_resolved_tool_activation(current, &step) {
            if touched_set.insert(step.var.clone()) {
                touched_vars.push(step.var.clone());
            }
            env.insert(step.var, new_value);
        }
    }

    for var in touched_vars {
        if let Some(value) = env.get(&var) {
            println_redacted(&format!("export {var}={}", shell_quote(value)));
        }
    }

    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Execute the `tools list` command.
///
/// Lists all tools configured in the lockfile.
///
/// # Errors
///
/// Returns an error if the lockfile is not found.
pub fn execute_tools_list() -> Result<(), CliError> {
    // Find the lockfile
    let lockfile_path = find_lockfile(None).ok_or_else(|| {
        CliError::config_with_help(
            "No cuenv.lock found",
            "Run 'cuenv sync lock' to create the lockfile",
        )
    })?;

    // Load the lockfile
    let lockfile = Lockfile::load(&lockfile_path)
        .map_err(|e| CliError::other(format!("Failed to load lockfile: {e}")))?
        .ok_or_else(|| {
            CliError::config_with_help(
                "Lockfile is empty",
                "Run 'cuenv sync lock' to populate the lockfile",
            )
        })?;

    for line in render_tools_list(
        &lockfile,
        &lockfile_path,
        &cuenv_core::lockfile::current_platform(),
    ) {
        println_redacted(&line);
    }

    Ok(())
}

/// Find the lockfile by walking up from the given directory (or current directory).
fn find_lockfile(start_path: Option<&Path>) -> Option<PathBuf> {
    let mut current = start_path
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::current_dir().ok())?;
    loop {
        let lockfile_path = current.join(LOCKFILE_NAME);
        if lockfile_path.exists() {
            return Some(lockfile_path);
        }

        // Also check in cue.mod directory
        let cue_mod_lockfile = current.join("cue.mod").join(LOCKFILE_NAME);
        if cue_mod_lockfile.exists() {
            return Some(cue_mod_lockfile);
        }

        if !current.pop() {
            return None;
        }
    }
}

/// Find a lockfile scoped to the current project only.
///
/// Runtime commands like `task` and `exec` should not inherit an ancestor
/// workspace lockfile when the target project does not define one.
fn find_lockfile_in_project(project_path: &Path) -> Option<PathBuf> {
    let project_lockfile = project_path.join(LOCKFILE_NAME);
    if project_lockfile.exists() {
        return Some(project_lockfile);
    }

    let cue_mod_lockfile = project_path.join("cue.mod").join(LOCKFILE_NAME);
    if cue_mod_lockfile.exists() {
        return Some(cue_mod_lockfile);
    }

    None
}

fn find_runtime_lockfile(project_path: Option<&Path>) -> Option<PathBuf> {
    project_path.map_or_else(|| find_lockfile(None), find_lockfile_in_project)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::lockfile::LockedToolPlatform;
    use std::fs;

    #[test]
    fn test_find_lockfile_not_found() {
        // Save and restore CWD to avoid breaking parallel tests
        let original_cwd = std::env::current_dir().unwrap();

        // Create temp dir without lockfile
        let temp = tempfile::tempdir().unwrap();
        std::env::set_current_dir(temp.path()).unwrap();

        // Should return None when searching from temp dir
        let result = find_lockfile(None);

        // Restore CWD before assertions (in case of panic)
        std::env::set_current_dir(&original_cwd).unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn test_find_lockfile_in_project_checks_project_root() {
        let temp = tempfile::tempdir().unwrap();
        let lockfile_path = temp.path().join(LOCKFILE_NAME);
        fs::write(&lockfile_path, "").unwrap();

        let result = find_lockfile_in_project(temp.path());

        assert_eq!(result, Some(lockfile_path));
    }

    #[test]
    fn test_find_lockfile_in_project_does_not_walk_to_parent() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(LOCKFILE_NAME), "").unwrap();

        let project_dir = temp.path().join("nested-project");
        fs::create_dir_all(&project_dir).unwrap();

        let result = find_lockfile_in_project(&project_dir);

        assert!(result.is_none());
    }

    #[test]
    fn test_lockfile_entry_to_source_parses_url_source() {
        let locked = LockedToolPlatform {
            provider: "url".to_string(),
            digest: "sha256:abc".to_string(),
            source: serde_json::json!({
                "type": "url",
                "url": "https://example.com/tool.tar.gz",
                "extract": [{"kind": "bin", "path": "tool"}],
            }),
            size: None,
            dependencies: vec![],
        };

        let source = lockfile_entry_to_source("tool", "1.0.0", &locked).expect("parsed source");
        match source {
            ToolSource::Url { url, extract } => {
                assert_eq!(url, "https://example.com/tool.tar.gz");
                assert_eq!(
                    extract,
                    vec![ToolExtract::Bin {
                        path: "tool".to_string(),
                        as_name: None,
                    }]
                );
            }
            _ => panic!("expected URL source"),
        }
    }
}
