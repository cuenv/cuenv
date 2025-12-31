//! OCI lockfile sync provider.
//!
//! Resolves OCI image references to content-addressed digests and writes
//! them to `cuenv.lock` for hermetic binary resolution.
//!
//! For `#ToolsRuntime`, uses ToolProvider to resolve tools from multiple sources
//! (GitHub releases, Nix packages).

use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_core::lockfile::{
    ArtifactKind, LOCKFILE_NAME, LockedArtifact, LockedToolPlatform, Lockfile, PlatformData,
};
use cuenv_core::manifest::{Base, GitHubProviderConfig, Project, Runtime, SourceConfig, ToolSpec};
use cuenv_core::tools::{Platform as ToolPlatform, ResolvedTool, ToolRegistry, ToolSource};
use cuenv_tools_oci::{OciClient, Platform};
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, info, warn};

use crate::commands::CommandExecutor;
use crate::commands::sync::provider::{SyncMode, SyncOptions, SyncProvider, SyncResult};

/// Create a tool registry with available providers.
fn create_registry(flakes: HashMap<String, String>) -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    // Register Nix provider with flake references from config
    registry.register(cuenv_tools_nix::NixToolProvider::with_flakes(flakes));

    // Register GitHub provider
    registry.register(cuenv_tools_github::GitHubToolProvider::new());

    registry
}

/// Sync provider for OCI lockfile resolution.
pub struct LockSyncProvider;

#[async_trait]
impl SyncProvider for LockSyncProvider {
    fn name(&self) -> &'static str {
        "lock"
    }

    fn description(&self) -> &'static str {
        "Resolve OCI images and update lockfile"
    }

    fn has_config(&self, _manifest: &Base) -> bool {
        // OCI runtime config is on Project, not Base
        // We'll check during sync
        false
    }

    async fn sync_path(
        &self,
        path: &Path,
        package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        let check = options.mode == SyncMode::Check;
        let output = execute_lock_sync(path, package, check, executor).await?;
        Ok(SyncResult::success(output))
    }

    async fn sync_workspace(
        &self,
        package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        // For lock, workspace sync is the same as path sync at current dir
        // since the lockfile is at module root and aggregates all projects
        let check = options.mode == SyncMode::Check;
        let output = execute_lock_sync(Path::new("."), package, check, executor).await?;
        Ok(SyncResult::success(output))
    }
}

/// Collected tool specification from manifest.
#[derive(Debug, Clone)]
struct CollectedTool {
    name: String,
    version: String,
    source: Option<SourceConfig>,
    overrides: Vec<cuenv_core::manifest::SourceOverride>,
    platforms: Vec<String>,
}

/// Execute lock synchronization for a path.
///
/// Scans all projects in the CUE module, collects OCI image references,
/// resolves them to digests, and writes `cuenv.lock`.
///
/// For Tools runtime, uses ToolProvider to resolve each tool from
/// GitHub releases or Nix packages.
async fn execute_lock_sync(
    path: &Path,
    _package: &str,
    check: bool,
    executor: &CommandExecutor,
) -> Result<String> {
    // Collect all OCI artifacts and tools from projects
    // Note: We collect all data before async operations to avoid holding
    // the module guard across await points (MutexGuard is not Send).
    let (
        lockfile_path,
        image_platforms,
        all_platforms,
        collected_tools,
        tools_platforms,
        collected_flakes,
        github_config,
    ) = {
        let module = executor.get_module(path)?;
        let module_root = module.root.clone();
        let lockfile_path = module_root.join(LOCKFILE_NAME);

        let mut image_platforms: HashMap<String, Vec<String>> = HashMap::new();
        let mut all_platforms: Vec<String> = Vec::new();
        let mut collected_tools: Vec<CollectedTool> = Vec::new();
        let mut tools_platforms: Vec<String> = Vec::new();
        let mut collected_flakes: HashMap<String, String> = HashMap::new();
        let mut github_config: Option<GitHubProviderConfig> = None;

        for instance in module.projects() {
            // Deserialize the instance to get the Project struct
            let project: Project = match instance.deserialize() {
                Ok(p) => p,
                Err(e) => {
                    warn!(error = %e, "Failed to deserialize project, skipping");
                    continue;
                }
            };

            match &project.runtime {
                // Handle OCI runtime (legacy)
                Some(Runtime::Oci(oci_runtime)) => {
                    // Collect platforms from this project's config
                    let resolve_platforms = &oci_runtime.platforms;
                    if resolve_platforms.is_empty() {
                        return Err(cuenv_core::Error::configuration(format!(
                            "Project '{}' uses OCI runtime but has no platforms configured",
                            project.name
                        )));
                    }

                    // Track all platforms for summary
                    for platform in resolve_platforms {
                        if !all_platforms.contains(platform) {
                            all_platforms.push(platform.clone());
                        }
                    }

                    // Process all images (unified API - everything is an image)
                    for image_spec in &oci_runtime.images {
                        let image = &image_spec.image;

                        let platforms = image_platforms.entry(image.clone()).or_default();
                        for platform in resolve_platforms {
                            if !platforms.contains(platform) {
                                platforms.push(platform.clone());
                            }
                        }
                    }
                }
                // Handle Tools runtime (new)
                Some(Runtime::Tools(tools_runtime)) => {
                    let resolve_platforms = &tools_runtime.platforms;
                    if resolve_platforms.is_empty() {
                        return Err(cuenv_core::Error::configuration(format!(
                            "Project '{}' uses Tools runtime but has no platforms configured",
                            project.name
                        )));
                    }

                    // Track all platforms for summary
                    for platform in resolve_platforms {
                        if !tools_platforms.contains(platform) {
                            tools_platforms.push(platform.clone());
                        }
                    }

                    // Collect flakes for Nix tool resolution
                    for (name, url) in &tools_runtime.flakes {
                        collected_flakes.insert(name.clone(), url.clone());
                    }

                    // Collect GitHub provider config (first one wins)
                    if github_config.is_none() {
                        github_config.clone_from(&tools_runtime.github);
                    }

                    // Collect all tools
                    for (name, spec) in &tools_runtime.tools {
                        let (version, source, overrides) = match spec {
                            ToolSpec::Version(v) => (v.clone(), None, vec![]),
                            ToolSpec::Full(config) => (
                                config.version.clone(),
                                config.source.clone(),
                                config.overrides.clone(),
                            ),
                        };

                        collected_tools.push(CollectedTool {
                            name: name.clone(),
                            version,
                            source,
                            overrides,
                            platforms: resolve_platforms.clone(),
                        });
                    }
                }
                // Skip projects without OCI/Tools runtime
                Some(_) | None => {}
            }
        }

        (
            lockfile_path,
            image_platforms,
            all_platforms,
            collected_tools,
            tools_platforms,
            collected_flakes,
            github_config,
        )
    };
    // Module guard is now dropped, we can safely use async operations

    // Resolve GitHub token if configured
    let github_token = if let Some(ref cfg) = github_config {
        if let Some(ref secret) = cfg.token {
            match secret.resolve().await {
                Ok(token) => Some(token),
                Err(e) => {
                    warn!(error = %e, "Failed to resolve GitHub token, continuing without authentication");
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    if image_platforms.is_empty() && collected_tools.is_empty() {
        return Ok("No OCI artifacts or tools found in any project.".to_string());
    }

    info!(
        "Resolving {} images for {} platforms",
        image_platforms.len(),
        all_platforms.len()
    );

    let client = OciClient::new();
    let mut artifacts: Vec<LockedArtifact> = Vec::new();

    // Process OCI images
    for (image, platforms) in &image_platforms {
        debug!(%image, ?platforms, "Resolving image");

        let mut platforms_map = HashMap::new();
        for platform_str in platforms {
            let platform = Platform::parse(platform_str).ok_or_else(|| {
                cuenv_core::Error::configuration(format!(
                    "Invalid platform '{}': expected format 'os-arch' (e.g., 'darwin-arm64')",
                    platform_str
                ))
            })?;

            let resolved = client.resolve_digest(image, &platform).await.map_err(|e| {
                cuenv_core::Error::configuration(format!(
                    "Failed to resolve '{}' for platform '{}': {}",
                    image, platform_str, e
                ))
            })?;

            debug!(%image, %platform_str, digest = %resolved.digest, "Resolved digest");

            platforms_map.insert(
                platform_str.clone(),
                PlatformData {
                    digest: resolved.digest,
                    size: None,
                },
            );
        }

        artifacts.push(LockedArtifact {
            kind: ArtifactKind::Image {
                image: (*image).clone(),
            },
            platforms: platforms_map,
        });
    }

    // Process Tools runtime
    let mut lockfile = Lockfile::new();

    if !collected_tools.is_empty() {
        info!(
            "Resolving {} tools for {} platforms",
            collected_tools.len(),
            tools_platforms.len()
        );

        let registry = create_registry(collected_flakes);

        for tool in &collected_tools {
            debug!(name = %tool.name, version = %tool.version, "Resolving tool");

            for platform_str in &tool.platforms {
                let platform = ToolPlatform::parse(platform_str).ok_or_else(|| {
                    cuenv_core::Error::configuration(format!(
                        "Invalid platform '{}': expected format 'os-arch' (e.g., 'darwin-arm64')",
                        platform_str
                    ))
                })?;

                // Determine source for this platform
                let source_config =
                    resolve_source_for_platform(tool.source.as_ref(), &tool.overrides, &platform)
                        .ok_or_else(|| {
                        cuenv_core::Error::configuration(format!(
                            "Tool '{}' has no source configured for platform '{}'. \
                                 Specify a source (github, nix, or oci) in your tool definition.",
                            tool.name, platform_str
                        ))
                    })?;

                // Convert SourceConfig to ToolSource
                let (provider_name, _tool_source, config) =
                    source_config_to_tool_source(&tool.name, &tool.version, &source_config);

                // Get the provider
                let Some(provider) = registry.get(&provider_name) else {
                    return Err(cuenv_core::Error::configuration(format!(
                        "No provider '{}' registered for tool '{}'",
                        provider_name, tool.name
                    )));
                };

                // Resolve the tool (pass GitHub token for authenticated API access)
                let resolved = provider
                    .resolve_with_token(
                        &tool.name,
                        &tool.version,
                        &platform,
                        &config,
                        github_token.as_deref(),
                    )
                    .await
                    .map_err(|e| {
                        cuenv_core::Error::configuration(format!(
                            "Failed to resolve tool '{}' for platform '{}': {}",
                            tool.name, platform_str, e
                        ))
                    })?;

                debug!(
                    tool = %tool.name,
                    %platform_str,
                    provider = %provider_name,
                    "Resolved tool"
                );

                // Add to lockfile
                // Use resolved.source instead of original config to capture expanded templates
                // (e.g., GitHub tag "bun-v{version}" becomes "bun-v1.3.5")
                let resolved_source = serde_json::to_value(&resolved.source).map_err(|e| {
                    cuenv_core::Error::configuration(format!(
                        "Failed to serialize resolved source for '{}': {}",
                        tool.name, e
                    ))
                })?;
                let locked_platform = LockedToolPlatform {
                    provider: provider_name.clone(),
                    digest: format!("sha256:{}", compute_tool_digest(&resolved)),
                    source: resolved_source,
                    size: None,
                    dependencies: vec![],
                };

                // Use the resolved version from the provider
                lockfile
                    .upsert_tool_platform(
                        &tool.name,
                        &resolved.version,
                        platform_str,
                        locked_platform,
                    )
                    .map_err(|e| {
                        cuenv_core::Error::configuration(format!(
                            "Failed to add tool '{}' to lockfile: {}",
                            tool.name, e
                        ))
                    })?;
            }
        }
    }

    // Add legacy artifacts
    lockfile.artifacts = artifacts;

    if check {
        // Check mode: compare against existing lockfile
        match Lockfile::load(&lockfile_path)? {
            Some(existing) => {
                if existing == lockfile {
                    Ok("Lockfile is up to date.".to_string())
                } else {
                    Err(cuenv_core::Error::configuration(
                        "Lockfile is out of date. Run 'cuenv sync lock' to update.",
                    ))
                }
            }
            None => Err(cuenv_core::Error::configuration(
                "No lockfile found. Run 'cuenv sync lock' to create one.",
            )),
        }
    } else {
        // Write mode: save the lockfile
        lockfile.save(&lockfile_path)?;

        // Ensure Nix profile is populated for current platform
        let lockfile_dir = lockfile_path.parent().unwrap_or(Path::new("."));
        if has_nix_tools(&lockfile) {
            cuenv_tools_nix::profile::ensure_profile(lockfile_dir, &lockfile).await?;
        }

        let image_count = lockfile.artifacts.len();
        let tools_count = lockfile.tools.len();

        let mut summary = Vec::new();
        if image_count > 0 {
            summary.push(format!(
                "{} images for [{}]",
                image_count,
                all_platforms.join(", ")
            ));
        }
        if tools_count > 0 {
            summary.push(format!(
                "{} tools for [{}]",
                tools_count,
                tools_platforms.join(", ")
            ));
        }

        Ok(format!(
            "Wrote {} to {}",
            summary.join(", "),
            lockfile_path.display()
        ))
    }
}

/// Check if the lockfile contains any Nix tools for the current platform.
fn has_nix_tools(lockfile: &Lockfile) -> bool {
    let platform = ToolPlatform::current().to_string();
    lockfile.tools.values().any(|tool| {
        tool.platforms
            .get(&platform)
            .is_some_and(|p| p.provider == "nix")
    })
}

/// Resolve the source configuration for a specific platform.
///
/// Applies overrides based on OS and architecture, returning the
/// appropriate source configuration. Returns None if no source is configured.
fn resolve_source_for_platform(
    default_source: Option<&SourceConfig>,
    overrides: &[cuenv_core::manifest::SourceOverride],
    platform: &ToolPlatform,
) -> Option<SourceConfig> {
    // Check overrides first (most specific match wins)
    let mut best_match: Option<&SourceConfig> = None;
    let mut best_specificity = 0;

    for override_config in overrides {
        let os_matches = override_config
            .os
            .as_ref()
            .map_or(true, |os| os == &platform.os.to_string());
        let arch_matches = override_config
            .arch
            .as_ref()
            .map_or(true, |arch| arch == &platform.arch.to_string());

        if os_matches && arch_matches {
            let specificity =
                u8::from(override_config.os.is_some()) + u8::from(override_config.arch.is_some());
            if specificity > best_specificity {
                best_specificity = specificity;
                best_match = Some(&override_config.source);
            }
        }
    }

    best_match.cloned().or_else(|| default_source.cloned())
}

/// Convert SourceConfig to ToolSource and provider config.
fn source_config_to_tool_source(
    _tool_name: &str,
    version: &str,
    config: &SourceConfig,
) -> (String, ToolSource, serde_json::Value) {
    match config {
        SourceConfig::Oci { image, path } => (
            "oci".to_string(),
            ToolSource::Oci {
                image: image.clone(),
                path: path.clone(),
            },
            serde_json::json!({ "image": image, "path": path }),
        ),
        SourceConfig::GitHub {
            repo,
            tag_prefix,
            tag,
            asset,
            path,
        } => {
            let resolved_tag = tag
                .clone()
                .unwrap_or_else(|| format!("{}{}", tag_prefix, version));
            (
                "github".to_string(),
                ToolSource::GitHub {
                    repo: repo.clone(),
                    tag: resolved_tag.clone(),
                    asset: asset.clone(),
                    path: path.clone(),
                },
                serde_json::json!({
                    "repo": repo,
                    "tag": resolved_tag,
                    "asset": asset,
                    "path": path,
                }),
            )
        }
        SourceConfig::Nix {
            flake,
            package,
            output,
        } => (
            "nix".to_string(),
            ToolSource::Nix {
                flake: flake.clone(),
                package: package.clone(),
                output: output.clone(),
            },
            serde_json::json!({
                "flake": flake,
                "package": package,
                "output": output,
            }),
        ),
    }
}

/// Compute a SHA256 digest for a resolved tool (for lockfile).
///
/// This creates a deterministic content hash based on the tool's
/// identifying information (name, version, platform, source).
fn compute_tool_digest(resolved: &ResolvedTool) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(resolved.name.as_bytes());
    hasher.update(b":");
    hasher.update(resolved.version.as_bytes());
    hasher.update(b":");
    hasher.update(resolved.platform.to_string().as_bytes());
    hasher.update(b":");
    // Include source info for uniqueness
    hasher.update(format!("{:?}", resolved.source).as_bytes());
    format!("{:x}", hasher.finalize())
}
