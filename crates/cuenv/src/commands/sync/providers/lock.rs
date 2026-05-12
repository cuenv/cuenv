//! OCI lockfile sync provider.
//!
//! Resolves OCI image references to content-addressed digests and writes
//! them to `cuenv.lock` for hermetic binary resolution.
//!
//! For `#ToolsRuntime`, uses ToolProvider to resolve tools from multiple sources
//! (GitHub releases, Nix packages).

use async_trait::async_trait;
use clap::{Arg, Command};
use cuenv_ci::flake::FlakeLockAnalyzer;
use cuenv_core::Result;
use cuenv_core::lockfile::{
    ArtifactKind, LOCKFILE_NAME, LockedArtifact, LockedNixRuntime, LockedRuntime, LockedTool,
    LockedToolPlatform, Lockfile, PlatformData,
};
use cuenv_core::manifest::{
    Base, GitHubExtract, GitHubProviderConfig, NixRuntime, Project, Runtime, SourceConfig, ToolSpec,
};
use cuenv_core::tools::{
    Platform as ToolPlatform, ResolvedTool, ToolExtract, ToolRegistry, ToolResolveRequest,
    ToolSource,
};
use cuenv_tools_oci::{OciClient, Platform};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
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

    // Register URL provider
    registry.register(cuenv_tools_url::UrlToolProvider::new());

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
        let output = execute_lock_sync(path, package, options, executor, false).await?;
        Ok(SyncResult::success(output))
    }

    async fn sync_workspace(
        &self,
        _path: &Path,
        package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        let output = execute_lock_sync(Path::new("."), package, options, executor, true).await?;
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

/// Collected runtime lock metadata keyed by project path.
#[derive(Debug, Clone)]
struct CollectedRuntime {
    project_path: String,
    runtime: LockedRuntime,
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
    resolved_source_config: &serde_json::Value,
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

    // Compare source config to detect template changes (e.g. path fix)
    if locked_platform.source != *resolved_source_config {
        debug!(
            %platform_str,
            "Cache miss: source config changed"
        );
        return None;
    }

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
/// When `workspace` is `true`, scans all projects in the CUE module via
/// `discover_all_modules`, collecting OCI image references and tools from
/// every sub-project. When `workspace` is `false`, evaluates only the target
/// directory via `get_module` (path-local scope).
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
    workspace: bool,
) -> Result<String> {
    let check = options.mode == SyncMode::Check;
    // Collect all OCI artifacts and tools from projects
    // Note: We collect all data before async operations to avoid holding
    // the module guard across await points (MutexGuard is not Send).
    let (
        lockfile_path,
        image_platforms,
        all_platforms,
        scoped_project_paths,
        collected_runtimes,
        collected_tools,
        tools_platforms,
        collected_flakes,
        github_config,
    ) = {
        let module = if workspace {
            executor.discover_all_modules(path)?
        } else {
            executor.get_module(path)?
        };
        let module_root = module.root.clone();
        let lockfile_path = module_root.join(LOCKFILE_NAME);

        let mut image_platforms: HashMap<String, Vec<String>> = HashMap::new();
        let mut all_platforms: Vec<String> = Vec::new();
        let mut scoped_project_paths = Vec::new();
        let mut collected_runtimes = Vec::new();
        // Use HashMap for tool deduplication: same tool in multiple projects is resolved once
        let mut tools_map: HashMap<ToolIdentityKey, CollectedTool> = HashMap::new();
        let mut tools_platforms: Vec<String> = Vec::new();
        let mut collected_flakes: HashMap<String, String> = HashMap::new();
        let mut github_config: Option<GitHubProviderConfig> = None;

        for instance in module.projects() {
            scoped_project_paths.push(display_project_path(&instance.path));

            // Deserialize the instance to get the Project struct
            let project: Project = match instance.deserialize() {
                Ok(p) => p,
                Err(e) => {
                    warn!(error = %e, "Failed to deserialize project, skipping");
                    continue;
                }
            };

            match &project.runtime {
                Some(Runtime::Nix(nix_runtime)) => {
                    collected_runtimes.push(collect_nix_runtime_lock(
                        &module_root,
                        &instance.path,
                        nix_runtime,
                    )?);
                }
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
            scoped_project_paths,
            collected_runtimes,
            collected_tools,
            tools_platforms,
            collected_flakes,
            github_config,
        )
    };
    // Module guard is now dropped, we can safely use async operations

    // Load existing lockfile once so we can preserve metadata and compare in check mode.
    let existing_lockfile = Lockfile::load(&lockfile_path)?;
    debug!(
        path = %lockfile_path.display(),
        has_lockfile = existing_lockfile.is_some(),
        tools_count = existing_lockfile.as_ref().map_or(0, |l| l.tools.len()),
        "Loaded existing lockfile"
    );

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

    if workspace
        && image_platforms.is_empty()
        && collected_tools.is_empty()
        && collected_runtimes.is_empty()
    {
        let lockfile = seed_lockfile(existing_lockfile.as_ref(), false);
        if lockfile_has_entries(&lockfile) {
            return match options.mode {
                SyncMode::Check => {
                    if existing_lockfile.as_ref() == Some(&lockfile) {
                        Ok("Lockfile is up to date.".to_string())
                    } else {
                        Err(cuenv_core::Error::configuration(
                            "Lockfile is out of date. Run 'cuenv sync lock' to update generated lockfile sections.",
                        ))
                    }
                }
                SyncMode::DryRun => Ok(format!(
                    "Would update lockfile at {}",
                    lockfile_path.display()
                )),
                SyncMode::Write => {
                    lockfile.save(&lockfile_path)?;
                    Ok(format!("Updated lockfile at {}", lockfile_path.display()))
                }
            };
        }
        return reconcile_empty_lockfile_state(
            &lockfile_path,
            existing_lockfile.as_ref(),
            &options.mode,
        );
    }

    info!(
        "Resolving {} images for {} platforms",
        image_platforms.len(),
        all_platforms.len()
    );

    let client = OciClient::new();
    let path_local = !workspace;
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
    let mut lockfile = seed_lockfile(existing_lockfile.as_ref(), path_local);

    if path_local {
        for project_path in &scoped_project_paths {
            lockfile.runtimes.remove(project_path);
        }
    }

    for runtime in &collected_runtimes {
        lockfile.upsert_runtime(runtime.project_path.clone(), runtime.runtime.clone())?;
    }

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
                                 Specify a source (github, nix, rustup, url, or oci) in your tool definition.",
                            tool.name, platform_str
                        ))
                    })?;

                // Convert SourceConfig to ToolSource
                let (provider_name, _tool_source, config) =
                    source_config_to_tool_source(&tool.version, &source_config, &platform);

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
                } else if check {
                    debug!(tool = %tool.name, "Skipping cache: check mode");
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

    if workspace {
        lockfile.artifacts = artifacts;
    } else {
        for artifact in artifacts {
            lockfile.upsert_artifact(artifact)?;
        }
    }

    if !lockfile_has_entries(&lockfile) {
        return reconcile_empty_lockfile_state(
            &lockfile_path,
            existing_lockfile.as_ref(),
            &options.mode,
        );
    }

    if check {
        // Check mode: compare against existing lockfile
        match existing_lockfile.as_ref() {
            Some(existing) => {
                if existing == &lockfile {
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
        if existing_lockfile
            .as_ref()
            .is_some_and(|existing| existing == &lockfile)
        {
            return Ok("Lockfile is up to date.".to_string());
        }

        // Write mode: save the lockfile
        lockfile.save(&lockfile_path)?;

        // Ensure Nix profile is populated for current platform
        let lockfile_dir = lockfile_path.parent().unwrap_or(Path::new("."));
        if has_nix_tools(&lockfile) {
            cuenv_tools_nix::profile::ensure_profile(lockfile_dir, &lockfile).await?;
        }

        let image_count = lockfile.artifacts.len();
        let runtime_count = lockfile.runtimes.len();
        let tools_count = lockfile.tools.len();

        let mut summary = Vec::new();
        if image_count > 0 {
            summary.push(format!(
                "{} images for [{}]",
                image_count,
                all_platforms.join(", ")
            ));
        }
        if runtime_count > 0 {
            summary.push(format!("{} runtimes", runtime_count));
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

fn reconcile_empty_lockfile_state(
    lockfile_path: &Path,
    existing_lockfile: Option<&Lockfile>,
    mode: &SyncMode,
) -> Result<String> {
    match mode {
        SyncMode::Check => {
            if existing_lockfile.is_some() {
                return Err(cuenv_core::Error::configuration(
                    "Lockfile is out of date. Run 'cuenv sync lock' to remove the stale lockfile.",
                ));
            }

            Ok("Lockfile is up to date.".to_string())
        }
        SyncMode::DryRun => {
            if existing_lockfile.is_some() {
                return Ok(format!(
                    "Would remove stale lockfile at {}",
                    lockfile_path.display()
                ));
            }

            Ok("Lockfile is up to date.".to_string())
        }
        SyncMode::Write => {
            if existing_lockfile.is_some() {
                std::fs::remove_file(lockfile_path).map_err(|e| {
                    cuenv_core::Error::configuration(format!(
                        "Failed to remove stale lockfile at {}: {}",
                        lockfile_path.display(),
                        e
                    ))
                })?;
                return Ok(format!(
                    "Removed stale lockfile at {}",
                    lockfile_path.display()
                ));
            }

            Ok("Lockfile is up to date.".to_string())
        }
    }
}

fn seed_lockfile(existing: Option<&Lockfile>, preserve_existing: bool) -> Lockfile {
    if preserve_existing {
        return existing.cloned().unwrap_or_else(Lockfile::new);
    }

    let mut lockfile = Lockfile::new();
    if let Some(existing) = existing {
        lockfile
            .tools_activation
            .clone_from(&existing.tools_activation);
        lockfile.vcs.clone_from(&existing.vcs);
    }
    lockfile
}

fn lockfile_has_entries(lockfile: &Lockfile) -> bool {
    !lockfile.runtimes.is_empty()
        || !lockfile.tools.is_empty()
        || !lockfile.vcs.is_empty()
        || !lockfile.artifacts.is_empty()
}

fn collect_nix_runtime_lock(
    module_root: &Path,
    instance_path: &Path,
    runtime: &NixRuntime,
) -> Result<CollectedRuntime> {
    let project_root = module_root.join(instance_path);
    let flake_root = resolve_local_flake_root(&project_root, &runtime.flake).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "Nix runtime flake '{}' is not a local path. cuenv sync requires a local flake so it can lock the checked-in flake.lock.",
            runtime.flake
        ))
    })?;

    let flake_lock_path = flake_root.join("flake.lock");
    let analyzer = FlakeLockAnalyzer::from_path(&flake_lock_path).map_err(|e| {
        cuenv_core::Error::configuration(format!(
            "Failed to read Nix runtime lockfile for project '{}': {}",
            display_project_path(instance_path),
            e
        ))
    })?;

    let analysis = analyzer.analyze();
    if !analysis.is_pure {
        let issues = analysis
            .unlocked_inputs
            .iter()
            .map(|input| format!("{} ({})", input.name, input.reason))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(cuenv_core::Error::configuration(format!(
            "Nix runtime flake.lock for project '{}' is not fully locked: {}",
            display_project_path(instance_path),
            issues
        )));
    }

    let relative_lockfile = relative_to_module_root(module_root, &flake_lock_path);

    Ok(CollectedRuntime {
        project_path: display_project_path(instance_path),
        runtime: LockedRuntime::Nix(LockedNixRuntime {
            flake: runtime.flake.clone(),
            output: runtime.output.clone(),
            digest: analysis.locked_digest,
            lockfile: relative_lockfile,
        }),
    })
}

fn display_project_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if path.is_empty() || path == "." {
        ".".to_string()
    } else {
        path.into_owned()
    }
}

fn relative_to_module_root(module_root: &Path, path: &Path) -> String {
    path.strip_prefix(module_root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn resolve_local_flake_root(project_root: &Path, flake: &str) -> Option<PathBuf> {
    let local_path = flake
        .strip_prefix("path:")
        .map_or_else(|| local_flake_path(flake), local_flake_path)?;

    if local_path.is_absolute() {
        Some(local_path)
    } else {
        Some(project_root.join(local_path))
    }
}

fn local_flake_path(reference: &str) -> Option<PathBuf> {
    if reference.is_empty() {
        return Some(PathBuf::from("."));
    }

    if reference.starts_with('/')
        || reference == "."
        || reference == ".."
        || reference.starts_with("./")
        || reference.starts_with("../")
    {
        return Some(PathBuf::from(reference));
    }

    if reference.contains(':') {
        return None;
    }

    Some(PathBuf::from(reference))
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

/// Convert `SourceConfig` to `ToolSource` and provider config.
fn source_config_to_tool_source(
    version: &str,
    config: &SourceConfig,
    platform: &ToolPlatform,
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
            extract,
        } => {
            let tag_template = tag
                .clone()
                .unwrap_or_else(|| format!("{}{}", tag_prefix, version));
            let resolved_tag = expand_source_template(&tag_template, version, platform);
            let resolved_asset = expand_source_template(asset, version, platform);
            let resolve_context = ExtractResolveContext {
                version,
                platform,
                legacy_path: path.as_deref(),
            };
            let resolved_extract = resolve_extract_templates(extract, &resolve_context);
            (
                "github".to_string(),
                ToolSource::GitHub {
                    repo: repo.clone(),
                    tag: resolved_tag.clone(),
                    asset: resolved_asset.clone(),
                    extract: resolved_extract.clone(),
                },
                serde_json::json!({
                    "type": "github",
                    "repo": repo,
                    "tag": resolved_tag,
                    "asset": resolved_asset,
                    "extract": resolved_extract,
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
        SourceConfig::Url { url, path, extract } => {
            let resolved_url = expand_source_template(url, version, platform);
            let resolve_context = ExtractResolveContext {
                version,
                platform,
                legacy_path: path.as_deref(),
            };
            let resolved_extract = resolve_extract_templates(extract, &resolve_context);
            (
                "url".to_string(),
                ToolSource::Url {
                    url: resolved_url.clone(),
                    extract: resolved_extract.clone(),
                },
                serde_json::json!({
                    "type": "url",
                    "url": resolved_url,
                    "extract": resolved_extract,
                }),
            )
        }
    }
}

struct ExtractResolveContext<'a> {
    version: &'a str,
    platform: &'a ToolPlatform,
    legacy_path: Option<&'a str>,
}

fn resolve_extract_templates(
    extract: &[GitHubExtract],
    context: &ExtractResolveContext<'_>,
) -> Vec<ToolExtract> {
    let mut resolved: Vec<ToolExtract> = extract
        .iter()
        .map(|item| match item {
            GitHubExtract::Bin { path, as_name } => ToolExtract::Bin {
                path: expand_source_template(path, context.version, context.platform),
                as_name: as_name.clone(),
            },
            GitHubExtract::Lib { path, env } => ToolExtract::Lib {
                path: expand_source_template(path, context.version, context.platform),
                env: env.clone(),
            },
            GitHubExtract::Include { path } => ToolExtract::Include {
                path: expand_source_template(path, context.version, context.platform),
            },
            GitHubExtract::PkgConfig { path } => ToolExtract::PkgConfig {
                path: expand_source_template(path, context.version, context.platform),
            },
            GitHubExtract::File { path, env } => ToolExtract::File {
                path: expand_source_template(path, context.version, context.platform),
                env: env.clone(),
            },
        })
        .collect();

    if resolved.is_empty()
        && let Some(path) = context.legacy_path
    {
        let path = expand_source_template(path, context.version, context.platform);
        if path_looks_like_library(&path) {
            resolved.push(ToolExtract::Lib { path, env: None });
        } else {
            resolved.push(ToolExtract::Bin {
                path,
                as_name: None,
            });
        }
    }

    resolved
}

fn path_looks_like_library(path: &str) -> bool {
    std::path::Path::new(path).extension().is_some_and(|ext| {
        ext.eq_ignore_ascii_case("dylib")
            || ext.eq_ignore_ascii_case("so")
            || ext.eq_ignore_ascii_case("dll")
    }) || path.to_ascii_lowercase().contains(".so.")
}

fn expand_source_template(value: &str, version: &str, platform: &ToolPlatform) -> String {
    let os_str = match platform.os {
        cuenv_core::tools::Os::Darwin => "darwin",
        cuenv_core::tools::Os::Linux => "linux",
    };
    let arch_str = match platform.arch {
        cuenv_core::tools::Arch::Arm64 => "aarch64",
        cuenv_core::tools::Arch::X86_64 => "x86_64",
    };

    #[allow(clippy::literal_string_with_formatting_args)]
    {
        value
            .replace("{version}", version)
            .replace("{os}", os_str)
            .replace("{arch}", arch_str)
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

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::lockfile::{LOCKFILE_VERSION, LockedToolPlatform, LockedVcsDependency};
    use cuenv_core::manifest::SourceConfig;
    use cuenv_core::tools::{
        Arch, Os, ToolActivationOperation, ToolActivationSource, ToolActivationStep,
    };

    #[test]
    fn test_seed_lockfile_preserves_tools_activation_and_resets_generated_sections() {
        let mut existing = Lockfile::new();
        existing.version = 1;
        existing.tools.insert(
            "jq".to_string(),
            LockedTool {
                version: "1.7.1".to_string(),
                platforms: BTreeMap::from([(
                    "linux-x86_64".to_string(),
                    LockedToolPlatform {
                        provider: "github".to_string(),
                        digest: "sha256:abc".to_string(),
                        source: serde_json::json!({"repo": "jqlang/jq"}),
                        size: None,
                        dependencies: vec![],
                    },
                )]),
            },
        );
        existing.tools_activation.push(ToolActivationStep {
            var: "PATH".to_string(),
            op: ToolActivationOperation::Prepend,
            separator: ":".to_string(),
            from: ToolActivationSource::AllBinDirs,
        });
        existing.artifacts.push(LockedArtifact {
            kind: ArtifactKind::Image {
                image: "nginx:1.25-alpine".to_string(),
            },
            platforms: BTreeMap::from([(
                "linux-x86_64".to_string(),
                PlatformData {
                    digest: "sha256:def".to_string(),
                    size: None,
                },
            )]),
        });
        existing.vcs.insert(
            "lib".to_string(),
            LockedVcsDependency {
                url: "https://example.com/lib.git".to_string(),
                reference: "main".to_string(),
                commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
                tree: "89abcdef012345670123456789abcdef01234567".to_string(),
                vendor: true,
                path: "vendor/lib".to_string(),
                subdir: None,
                subtree: None,
            },
        );

        let seeded = seed_lockfile(Some(&existing), false);

        assert_eq!(seeded.version, LOCKFILE_VERSION);
        assert_eq!(seeded.tools_activation, existing.tools_activation);
        assert_eq!(seeded.vcs, existing.vcs);
        assert!(seeded.tools.is_empty());
        assert!(seeded.artifacts.is_empty());
    }

    #[test]
    fn test_seeded_lockfile_remains_equal_when_generated_sections_are_rebuilt() {
        let mut existing = Lockfile::new();
        existing.tools.insert(
            "jq".to_string(),
            LockedTool {
                version: "1.7.1".to_string(),
                platforms: BTreeMap::from([(
                    "linux-x86_64".to_string(),
                    LockedToolPlatform {
                        provider: "github".to_string(),
                        digest: "sha256:abc".to_string(),
                        source: serde_json::json!({"repo": "jqlang/jq"}),
                        size: None,
                        dependencies: vec![],
                    },
                )]),
            },
        );
        existing.tools_activation.push(ToolActivationStep {
            var: "PATH".to_string(),
            op: ToolActivationOperation::Prepend,
            separator: ":".to_string(),
            from: ToolActivationSource::AllBinDirs,
        });
        existing.artifacts.push(LockedArtifact {
            kind: ArtifactKind::Image {
                image: "nginx:1.25-alpine".to_string(),
            },
            platforms: BTreeMap::from([(
                "linux-x86_64".to_string(),
                PlatformData {
                    digest: "sha256:def".to_string(),
                    size: None,
                },
            )]),
        });

        let mut rebuilt = seed_lockfile(Some(&existing), false);
        rebuilt.tools = existing.tools.clone();
        rebuilt.artifacts = existing.artifacts.clone();

        assert_eq!(rebuilt, existing);
    }

    #[test]
    fn test_source_config_to_tool_source_expands_url_templates_for_cache_comparison() {
        let source = SourceConfig::Url {
            url: "https://example.com/tool-{version}-{os}-{arch}.tar.gz".to_string(),
            path: Some("tool-{os}-{arch}".to_string()),
            extract: vec![],
        };
        let platform = ToolPlatform::new(Os::Linux, Arch::Arm64);

        let (_, tool_source, source_json) =
            source_config_to_tool_source("1.2.3", &source, &platform);

        match tool_source {
            ToolSource::Url { url, extract } => {
                assert_eq!(url, "https://example.com/tool-1.2.3-linux-aarch64.tar.gz");
                assert_eq!(
                    extract,
                    vec![ToolExtract::Bin {
                        path: "tool-linux-aarch64".to_string(),
                        as_name: None,
                    }]
                );
            }
            _ => panic!("expected url source"),
        }

        assert_eq!(
            source_json,
            serde_json::json!({
                "type": "url",
                "url": "https://example.com/tool-1.2.3-linux-aarch64.tar.gz",
                "extract": [{"kind": "bin", "path": "tool-linux-aarch64"}],
            })
        );
    }
}
