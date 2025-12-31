//! Tools command implementations for multi-source tool management.
//!
//! This module provides commands for downloading, activating, and listing tools
//! from multiple sources (GitHub releases, Nix packages, OCI images).

use crate::cli::CliError;
use cuenv_core::lockfile::{LOCKFILE_NAME, Lockfile};
use cuenv_core::tools::{Platform, ToolOptions, ToolRegistry, ToolSource};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Create a tool registry with available providers.
fn create_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    // Register Nix provider
    registry.register(cuenv_tools_nix::NixToolProvider::new());

    // Register GitHub provider
    registry.register(cuenv_tools_github::GitHubToolProvider::new());

    // Register Rustup provider
    registry.register(cuenv_tools_rustup::RustupToolProvider::new());

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

    for (name, tool) in &lockfile.tools {
        let Some(locked) = tool.platforms.get(&platform_str) else {
            // Tool not available for this platform
            continue;
        };

        // Convert lockfile data to ToolSource
        let source = match locked.provider.as_str() {
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
    let Some(lockfile_path) = find_lockfile(project_path) else {
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
                tracing::warn!("Failed to download '{}': {} - continuing anyway", name, e);
            }
        }
    }

    if downloaded > 0 {
        tracing::info!("Downloaded {} tools", downloaded);
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
            let path = locked
                .source
                .get("path")
                .and_then(|v| v.as_str())
                .map(String::from);
            Some(ToolSource::GitHub {
                repo: repo.to_string(),
                tag: tag.to_string(),
                asset: asset.to_string(),
                path,
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
        _ => None,
    }
}

/// Tool environment paths for activation.
#[derive(Debug, Clone, Default)]
pub struct ToolPaths {
    /// Directories to prepend to PATH.
    pub bin_dirs: Vec<PathBuf>,
    /// Directories to prepend to library path (DYLD_LIBRARY_PATH or LD_LIBRARY_PATH).
    pub lib_dirs: Vec<PathBuf>,
}

impl ToolPaths {
    /// Check if there are any paths to add.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bin_dirs.is_empty() && self.lib_dirs.is_empty()
    }

    /// Get the PATH string to prepend (colon-separated).
    #[must_use]
    pub fn path_prepend(&self) -> Option<String> {
        if self.bin_dirs.is_empty() {
            None
        } else {
            Some(
                self.bin_dirs
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(":"),
            )
        }
    }

    /// Get the library path string to prepend (colon-separated).
    #[must_use]
    pub fn lib_path_prepend(&self) -> Option<String> {
        if self.lib_dirs.is_empty() {
            None
        } else {
            Some(
                self.lib_dirs
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(":"),
            )
        }
    }
}

/// Get tool paths from the lockfile for the current platform.
///
/// This function finds the lockfile, reads it, and returns the bin/lib directories
/// that should be added to PATH and library path for tool activation.
///
/// If `project_path` is provided, the lockfile search starts from that directory.
/// Otherwise, it starts from the current working directory.
///
/// Returns `Ok(None)` if no lockfile is found (not an error - just no tools to activate).
///
/// # Errors
///
/// Returns an error if the lockfile exists but cannot be read or parsed.
pub fn get_tool_paths(project_path: Option<&Path>) -> Result<Option<ToolPaths>, CliError> {
    // Find the lockfile - not finding one is not an error
    let Some(lockfile_path) = find_lockfile(project_path) else {
        return Ok(None);
    };

    // Load the lockfile
    let lockfile = Lockfile::load(&lockfile_path)
        .map_err(|e| CliError::other(format!("Failed to load lockfile: {e}")))?;

    let Some(lockfile) = lockfile else {
        // Empty lockfile - no tools to activate
        return Ok(None);
    };

    if lockfile.tools.is_empty() {
        return Ok(None);
    }

    // Get current platform
    let platform = Platform::current();
    let platform_str = platform.to_string();

    // Get default cache directory
    let cache_dir = ToolOptions::default().cache_dir();

    // Collect bin and lib directories
    let mut bin_dirs: HashSet<PathBuf> = HashSet::new();
    let mut lib_dirs: HashSet<PathBuf> = HashSet::new();

    // 1. Check for Nix profile in XDG cache (for Nix tools)
    let lockfile_dir = lockfile_path.parent().unwrap_or(Path::new("."));
    if let Ok(profile_path) = cuenv_tools_nix::profile::profile_path_for_project(lockfile_dir) {
        let bin = profile_path.join("bin");
        if bin.exists() {
            bin_dirs.insert(bin);
        }
        let lib = profile_path.join("lib");
        if lib.exists() {
            lib_dirs.insert(lib);
        }
    }

    // 2. Process non-Nix tools (cache-based or rustup)
    for (name, tool) in &lockfile.tools {
        let Some(locked) = tool.platforms.get(&platform_str) else {
            continue;
        };

        // Skip Nix tools - they use profile, not cache
        if locked.provider == "nix" {
            continue;
        }

        // Handle rustup tools specially - they live in ~/.rustup/toolchains/
        if locked.provider == "rustup" {
            if let Some(toolchain) = locked.source.get("toolchain").and_then(|v| v.as_str()) {
                let rustup_home = std::env::var("RUSTUP_HOME").map_or_else(
                    |_| {
                        dirs::home_dir()
                            .unwrap_or_else(|| PathBuf::from("."))
                            .join(".rustup")
                    },
                    PathBuf::from,
                );

                // Construct toolchain name with host triple
                let host_triple = format!(
                    "{}-{}",
                    match platform.arch {
                        cuenv_core::tools::Arch::Arm64 => "aarch64",
                        cuenv_core::tools::Arch::X86_64 => "x86_64",
                    },
                    match platform.os {
                        cuenv_core::tools::Os::Darwin => "apple-darwin",
                        cuenv_core::tools::Os::Linux => "unknown-linux-gnu",
                    }
                );
                let toolchain_name = format!("{toolchain}-{host_triple}");
                let toolchain_dir = rustup_home.join("toolchains").join(toolchain_name);

                let bin = toolchain_dir.join("bin");
                let lib = toolchain_dir.join("lib");

                if bin.exists() {
                    bin_dirs.insert(bin);
                }
                if lib.exists() {
                    lib_dirs.insert(lib);
                }
            }
            continue;
        }

        // Construct the tool directory based on provider (github, oci, etc.)
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

    if bin_dirs.is_empty() && lib_dirs.is_empty() {
        return Ok(None);
    }

    Ok(Some(ToolPaths {
        bin_dirs: bin_dirs.into_iter().collect(),
        lib_dirs: lib_dirs.into_iter().collect(),
    }))
}

/// Execute the `tools activate` command.
///
/// Outputs shell export statements to add tool binaries to PATH.
///
/// # Errors
///
/// Returns an error if the lockfile is not found.
pub fn execute_tools_activate() -> Result<(), CliError> {
    let tool_paths = get_tool_paths(None)?.ok_or_else(|| {
        CliError::config_with_help(
            "No cuenv.lock found or no tools configured",
            "Run 'cuenv sync lock' to create the lockfile",
        )
    })?;

    // Output library path modification first (dependencies must be available)
    if let Some(lib_path) = tool_paths.lib_path_prepend() {
        // Use appropriate library path variable for the platform
        #[cfg(target_os = "macos")]
        println!("export DYLD_LIBRARY_PATH=\"{lib_path}:$DYLD_LIBRARY_PATH\"");

        #[cfg(not(target_os = "macos"))]
        println!("export LD_LIBRARY_PATH=\"{lib_path}:$LD_LIBRARY_PATH\"");
    }

    // Output PATH modification
    if let Some(path) = tool_paths.path_prepend() {
        println!("export PATH=\"{path}:$PATH\"");
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

        // Should return None when searching from temp dir
        let result = find_lockfile(None);

        // Restore CWD before assertions (in case of panic)
        std::env::set_current_dir(&original_cwd).unwrap();

        assert!(result.is_none());
    }
}
