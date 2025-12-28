//! Tools command implementations for multi-source tool management.
//!
//! This module provides commands for downloading, activating, and listing tools
//! from multiple sources (Homebrew, OCI, GitHub, Nix).

use crate::cli::CliError;
use cuenv_core::lockfile::{LOCKFILE_NAME, Lockfile};
use cuenv_core::tools::{Platform, ToolOptions, ToolRegistry, ToolSource};
use std::collections::HashSet;
use std::path::PathBuf;

/// Create a tool registry with available providers.
fn create_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    // Register Homebrew provider (uses OCI crate internally for ghcr.io/homebrew)
    registry.register(cuenv_tools_homebrew::HomebrewToolProvider::new());

    // Register Nix provider
    registry.register(cuenv_tools_nix::NixToolProvider::new());

    // Register GitHub provider
    registry.register(cuenv_tools_github::GitHubToolProvider::new());

    // Note: OCI provider for generic images is not yet implemented.
    // The homebrew provider handles ghcr.io/homebrew/* images.

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
    let lockfile_path = find_lockfile().ok_or_else(|| {
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

    for (name, tool) in &lockfile.tools {
        let Some(locked) = tool.platforms.get(&platform_str) else {
            // Tool not available for this platform
            continue;
        };

        // Convert lockfile data to ToolSource
        let source = match locked.provider.as_str() {
            "homebrew" => {
                let formula = locked
                    .source
                    .get("formula")
                    .and_then(|v| v.as_str())
                    .unwrap_or(name);
                let image_ref = format!("ghcr.io/homebrew/core/{}:{}", formula, tool.version);
                ToolSource::Homebrew {
                    formula: formula.to_string(),
                    image_ref,
                }
            }
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
                ToolSource::Oci {
                    image: image.to_string(),
                    path: path.to_string(),
                }
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
                let path = locked
                    .source
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                ToolSource::GitHub {
                    repo: repo.to_string(),
                    tag: tag.to_string(),
                    asset: asset.to_string(),
                    path,
                }
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
                ToolSource::Nix {
                    flake: flake.to_string(),
                    package: package.to_string(),
                    output,
                }
            }
            _ => {
                eprintln!(
                    "Warning: Unknown provider '{}' for tool '{}'",
                    locked.provider, name
                );
                continue;
            }
        };

        // Get the provider
        let Some(provider) = registry.find_for_source(&source) else {
            eprintln!(
                "Warning: No provider found for source type of tool '{}'",
                name
            );
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
        println!("Downloading {} v{}...", name, tool.version);
        match provider.fetch(&resolved, &options).await {
            Ok(fetched) => {
                println!(
                    "  -> {} ({})",
                    fetched.binary_path.display(),
                    fetched.sha256
                );
                downloaded += 1;
            }
            Err(e) => {
                eprintln!("  Error downloading '{}': {}", name, e);
            }
        }
    }

    println!();
    println!(
        "Downloaded {} tools, {} already cached",
        downloaded, skipped
    );

    Ok(())
}

/// Execute the `tools activate` command.
///
/// Outputs shell export statements to add tool binaries to PATH.
///
/// # Errors
///
/// Returns an error if the lockfile is not found.
pub fn execute_tools_activate() -> Result<(), CliError> {
    // Find the lockfile
    let lockfile_path = find_lockfile().ok_or_else(|| {
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

    // Get default cache directory
    let cache_dir = ToolOptions::default().cache_dir();

    // Collect bin and lib directories
    let mut bin_dirs: HashSet<PathBuf> = HashSet::new();
    let mut lib_dirs: HashSet<PathBuf> = HashSet::new();

    for (name, tool) in &lockfile.tools {
        let Some(locked) = tool.platforms.get(&platform_str) else {
            continue;
        };

        // Construct the tool directory based on provider
        let tool_dir = cache_dir
            .join(&locked.provider)
            .join(name)
            .join(&tool.version);

        let bin = tool_dir.join("bin");
        let lib = tool_dir.join("lib");

        if bin.exists() {
            bin_dirs.insert(bin);
        }
        if lib.exists() {
            lib_dirs.insert(lib);
        }
    }

    // Output library path modification first (dependencies must be available)
    if !lib_dirs.is_empty() {
        let lib_path: Vec<String> = lib_dirs.iter().map(|p| p.display().to_string()).collect();
        let lib_path_str = lib_path.join(":");

        // Use appropriate library path variable for the platform
        #[cfg(target_os = "macos")]
        println!(
            "export DYLD_LIBRARY_PATH=\"{}:$DYLD_LIBRARY_PATH\"",
            lib_path_str
        );

        #[cfg(not(target_os = "macos"))]
        println!(
            "export LD_LIBRARY_PATH=\"{}:$LD_LIBRARY_PATH\"",
            lib_path_str
        );
    }

    // Output PATH modification
    if !bin_dirs.is_empty() {
        let path_additions: Vec<String> =
            bin_dirs.iter().map(|p| p.display().to_string()).collect();
        println!("export PATH=\"{}:$PATH\"", path_additions.join(":"));
    }

    Ok(())
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
    let lockfile_path = find_lockfile().ok_or_else(|| {
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

    // Get current platform for highlighting
    let current_platform = cuenv_core::lockfile::current_platform();

    if lockfile.tools.is_empty() {
        println!("No tools configured.");
        println!();
        println!("To add tools, create a runtime in your env.cue:");
        println!();
        println!("  runtime: #ToolsRuntime & {{");
        println!("      platforms: [\"darwin-arm64\", \"linux-x86_64\"]");
        println!("      tools: {{");
        println!("          jq: \"1.7.1\"");
        println!("          yq: \"4.44.6\"");
        println!("      }}");
        println!("  }}");
        return Ok(());
    }

    println!("Configured tools:");
    println!();

    // Sort tools by name
    let mut tools: Vec<_> = lockfile.tools.iter().collect();
    tools.sort_by_key(|(name, _)| *name);

    for (name, tool) in tools {
        println!("  {} v{}", name, tool.version);

        // Show platforms
        for (platform, locked) in &tool.platforms {
            let marker = if platform == &current_platform {
                " (current)"
            } else {
                ""
            };
            println!(
                "    - {}: {} ({}){}",
                platform,
                locked.provider,
                &locked.digest[..20],
                marker
            );
        }
    }

    println!();
    println!(
        "Total: {} tools, {} platforms",
        lockfile.tools.len(),
        lockfile
            .tools
            .values()
            .map(|t| t.platforms.len())
            .sum::<usize>()
    );

    Ok(())
}

/// Find the lockfile by walking up from current directory.
fn find_lockfile() -> Option<PathBuf> {
    let mut current = std::env::current_dir().ok()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_lockfile_not_found() {
        // Save and restore CWD to avoid breaking parallel tests
        let original_cwd = std::env::current_dir().unwrap();

        // Create temp dir without lockfile
        let temp = tempfile::tempdir().unwrap();
        std::env::set_current_dir(temp.path()).unwrap();

        // Should return None
        let result = find_lockfile();

        // Restore CWD before assertions (in case of panic)
        std::env::set_current_dir(&original_cwd).unwrap();

        assert!(result.is_none());
    }
}
