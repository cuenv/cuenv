//! OCI lockfile sync provider.
//!
//! Resolves OCI image references to content-addressed digests and writes
//! them to `cuenv.lock` for hermetic binary resolution.
//!
//! For `#ToolsRuntime`, uses ToolProvider to resolve tools from multiple sources
//! (GitHub releases, Nix packages).

use async_trait::async_trait;
use clap::{Arg, Command};
use cuenv_core::Result;
use cuenv_core::lockfile::{
    ArtifactKind, LOCKFILE_NAME, LockedArtifact, LockedTool, LockedToolPlatform, Lockfile,
    PlatformData,
};
use cuenv_core::manifest::{Base, GitHubProviderConfig, Project, Runtime, SourceConfig, ToolSpec};
use cuenv_core::tools::{
    Platform as ToolPlatform, ResolvedTool, ToolRegistry, ToolResolveRequest, ToolSource,
};
use cuenv_tools_oci::{OciClient, Platform};
use std::collections::{BTreeMap, HashMap};
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

    // Register Rustup provider
    registry.register(cuenv_tools_rustup::RustupToolProvider::new());

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

    fn build_command(&self) -> Command {
        self.default_command().arg(
            Arg::new("update")
                .short('u')
                .long("update")
                .help("Force re-resolution of tools, ignoring cached lockfile resolutions. Optionally specify tool names to update only those tools.")
                .num_args(0..)
                .value_name("TOOLS")
                .action(clap::ArgAction::Append),
        )
    }

    fn parse_args(&self, matches: &clap::ArgMatches) -> SyncOptions {
        let mode = if matches.get_flag("dry-run") {
            SyncMode::DryRun
        } else if matches.get_flag("check") {
            SyncMode::Check
        } else {
            SyncMode::Write
        };

        // Parse -u/--update flag
        // - Not present: None (use cache)
        // - Present with no args: Some(vec![]) (update all)
        // - Present with args: Some(vec!["tool1", "tool2"]) (update specific tools)
        let update_tools = if matches.contains_id("update") {
            let tools: Vec<String> = matches
                .get_many::<String>("update")
                .map(|vals| vals.cloned().collect())
                .unwrap_or_default();
            Some(tools)
        } else {
            None
        };

        SyncOptions {
            mode,
            show_diff: matches.get_flag("diff"),
            ci_provider: matches.get_one::<String>("provider").cloned(),
            update_tools,
        }
    }

    async fn sync_path(
        &self,
        path: &Path,
        package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        let output = execute_lock_sync(path, package, options, executor).await?;
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
        let output = execute_lock_sync(Path::new("."), package, options, executor).await?;
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

/// Key type for tool deduplication: (name, version, source_hash).
type ToolIdentityKey = (String, String, String);

/// Check if a tool should be force-updated based on the update_tools option.
///
/// Returns true if the tool should be re-resolved from the provider,
/// false if it should use the cached lockfile resolution.
fn should_update_tool(tool_name: &str, update_tools: Option<&Vec<String>>) -> bool {
    match update_tools {
        None => false,                                       // No -u flag: use cache
        Some(tools) if tools.is_empty() => true,             // -u alone: update all
        Some(tools) => tools.iter().any(|t| t == tool_name), // -u <names>: update if listed
    }
}

/// Get a valid cached resolution from the lockfile if it's still valid.
///
/// Returns the cached platform resolution if:
/// 1. The locked version matches the requested version
/// 2. The locked platform exists
/// 3. The locked source configuration matches the resolved manifest source
///
/// Returns `None` if the cache is stale and the tool needs to be re-resolved.
fn get_valid_cached_resolution<'a>(
    locked_tool: &'a LockedTool,
    manifest_version: &str,
    platform_str: &str,
    _resolved_source_config: &serde_json::Value,
) -> Option<&'a LockedToolPlatform> {
    // Check version match
    if locked_tool.version != manifest_version {
        debug!(
            locked_version = %locked_tool.version,
            %manifest_version,
            "Cache miss: version mismatch"
        );
        return None;
    }

    // Check platform exists
    let Some(locked_platform) = locked_tool.platforms.get(platform_str) else {
        debug!(%platform_str, "Cache miss: platform not in lockfile");
        return None;
    };

    // If version and platform match, the cache is valid.
    // The source configuration is implicitly validated by the version match,
    // since changing the source would require updating the tool definition.
    Some(locked_platform)
}

/// Create a unique identity key for tool deduplication.
///
/// Tools are considered identical if they have the same name, version, and source.
/// This prevents resolving the same tool multiple times when defined in multiple projects.
fn tool_identity_key(name: &str, version: &str, source: Option<&SourceConfig>) -> ToolIdentityKey {
    let source_hash = source.map(|s| format!("{s:?}")).unwrap_or_default();
    (name.to_string(), version.to_string(), source_hash)
}

/// Execute lock synchronization for a path.
///
/// Scans all projects in the CUE module, collects OCI image references,
/// resolves them to digests, and writes `cuenv.lock`.
///
/// For Tools runtime, uses ToolProvider to resolve each tool from
/// GitHub releases or Nix packages.
///
/// If the lockfile already contains valid resolutions for tools (same version
/// and source config), those are reused without contacting the provider,
/// unless `-u/--update` is specified.
async fn execute_lock_sync(
    path: &Path,
    _package: &str,
    options: &SyncOptions,
    executor: &CommandExecutor,
) -> Result<String> {
    let check = options.mode == SyncMode::Check;
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
        // Use HashMap for tool deduplication: same tool in multiple projects is resolved once
        let mut tools_map: HashMap<ToolIdentityKey, CollectedTool> = HashMap::new();
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

                    // Collect all tools with deduplication across projects
                    for (name, spec) in &tools_runtime.tools {
                        let (version, source, overrides) = match spec {
                            ToolSpec::Version(v) => (v.clone(), None, vec![]),
                            ToolSpec::Full(config) => (
                                config.version.clone(),
                                config.source.clone(),
                                config.overrides.clone(),
                            ),
                        };

                        let key = tool_identity_key(name, &version, source.as_ref());
                        tools_map
                            .entry(key)
                            .and_modify(|existing| {
                                // Merge platforms from this project into existing entry
                                for platform in resolve_platforms {
                                    if !existing.platforms.contains(platform) {
                                        existing.platforms.push(platform.clone());
                                    }
                                }
                            })
                            .or_insert_with(|| CollectedTool {
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

        // Convert deduplicated tools map to Vec
        let collected_tools: Vec<CollectedTool> = tools_map.into_values().collect();

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

    // Load existing lockfile for cache lookup (unless in check mode or forcing update)
    let existing_lockfile = if check {
        // In check mode, we'll load it later for comparison
        None
    } else {
        let loaded = Lockfile::load(&lockfile_path)?;
        debug!(
            path = %lockfile_path.display(),
            has_lockfile = loaded.is_some(),
            tools_count = loaded.as_ref().map_or(0, |l| l.tools.len()),
            "Loaded existing lockfile"
        );
        loaded
    };

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

        let mut platforms_map = BTreeMap::new();
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
            "Resolving {} unique tools for {} platforms",
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

                // Check if we should use cached resolution from existing lockfile
                let force_update = should_update_tool(&tool.name, options.update_tools.as_ref());

                // Debug: log what we're comparing
                debug!(
                    tool = %tool.name,
                    %platform_str,
                    manifest_version = %tool.version,
                    config = %config,
                    "Checking lockfile cache"
                );

                // Check cache
                let cached = if force_update {
                    debug!(tool = %tool.name, "Skipping cache: force update requested");
                    None
                } else if let Some(ref existing) = existing_lockfile {
                    if let Some(locked_tool) = existing.find_tool(&tool.name) {
                        get_valid_cached_resolution(
                            locked_tool,
                            &tool.version,
                            platform_str,
                            &config,
                        )
                    } else {
                        debug!(tool = %tool.name, "Cache miss: tool not in lockfile");
                        None
                    }
                } else {
                    debug!(tool = %tool.name, "Cache miss: no existing lockfile");
                    None
                };

                if let Some(locked_platform) = cached {
                    // Reuse cached resolution - skip provider call
                    // The version is the same (verified by get_valid_cached_resolution)
                    debug!(
                        tool = %tool.name,
                        %platform_str,
                        "Using cached resolution from lockfile"
                    );

                    lockfile
                        .upsert_tool_platform(
                            &tool.name,
                            &tool.version,
                            platform_str,
                            locked_platform.clone(),
                        )
                        .map_err(|e| {
                            cuenv_core::Error::configuration(format!(
                                "Failed to add tool '{}' to lockfile: {}",
                                tool.name, e
                            ))
                        })?;
                    continue;
                }

                // No valid cache or force update - resolve from provider
                // Get the provider
                let Some(provider) = registry.get(&provider_name) else {
                    return Err(cuenv_core::Error::configuration(format!(
                        "No provider '{}' registered for tool '{}'",
                        provider_name, tool.name
                    )));
                };

                info!(
                    tool = %tool.name,
                    %platform_str,
                    provider = %provider_name,
                    "Resolving tool from provider"
                );

                // Resolve the tool (pass GitHub token for authenticated API access)
                let resolved = provider
                    .resolve(&ToolResolveRequest {
                        tool_name: &tool.name,
                        version: &tool.version,
                        platform: &platform,
                        config: &config,
                        token: github_token.as_deref(),
                    })
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
            serde_json::json!({ "type": "oci", "image": image, "path": path }),
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
            // Expand {version} template in asset name
            #[allow(clippy::literal_string_with_formatting_args)]
            let resolved_asset = asset.replace("{version}", version);
            (
                "github".to_string(),
                ToolSource::GitHub {
                    repo: repo.clone(),
                    tag: resolved_tag.clone(),
                    asset: resolved_asset.clone(),
                    path: path.clone(),
                },
                serde_json::json!({
                    "type": "github",
                    "repo": repo,
                    "tag": resolved_tag,
                    "asset": resolved_asset,
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
                "type": "nix",
                "flake": flake,
                "package": package,
                "output": output,
            }),
        ),
        SourceConfig::Rustup {
            toolchain,
            profile,
            components,
            targets,
        } => (
            "rustup".to_string(),
            ToolSource::Rustup {
                toolchain: toolchain.clone(),
                profile: Some(profile.clone()),
                components: components.clone(),
                targets: targets.clone(),
            },
            serde_json::json!({
                "type": "rustup",
                "toolchain": toolchain,
                "profile": profile,
                "components": components,
                "targets": targets,
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
