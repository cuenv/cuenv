//! OCI lockfile sync provider.
//!
//! Resolves OCI image references to content-addressed digests and writes
//! them to `cuenv.lock` for hermetic binary resolution.
//!
//! For `#ToolsRuntime`, uses ToolProvider to resolve tools from multiple sources
//! (GitHub releases, Nix packages).

mod tool_resolution;

use async_trait::async_trait;
use clap::{Arg, Command};
use cuenv_ci::flake::FlakeLockAnalyzer;
use cuenv_core::Result;
use cuenv_core::lockfile::{
    ArtifactKind, LOCKFILE_NAME, LockedArtifact, LockedNixRuntime, LockedOciExtract, LockedRuntime,
    Lockfile, PlatformData,
};
use cuenv_core::manifest::{Base, GitHubProviderConfig, NixRuntime, Project, Runtime, ToolSpec};
use cuenv_tools_oci::{OciClient, Platform};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use crate::commands::CommandExecutor;
use crate::commands::sync::provider::{SyncMode, SyncOptions, SyncProvider, SyncResult};
use tool_resolution::{
    CollectedTool, ToolIdentityKey, ToolLockResolutionRequest, has_nix_tools, resolve_tool_locks,
    tool_identity_key,
};

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
        _package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        let output = execute_lock_sync(LockSyncRequest {
            path,
            options,
            executor,
            scope: LockSyncScope::Path,
        })
        .await?;
        Ok(SyncResult::success(output))
    }

    async fn sync_workspace(
        &self,
        _path: &Path,
        _package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        let output = execute_lock_sync(LockSyncRequest {
            path: Path::new("."),
            options,
            executor,
            scope: LockSyncScope::Workspace,
        })
        .await?;
        Ok(SyncResult::success(output))
    }
}

/// Collected runtime lock metadata keyed by project path.
#[derive(Debug, Clone)]
struct CollectedRuntime {
    project_path: String,
    runtime: LockedRuntime,
}

/// Collected OCI image specification.
///
/// Aggregates per-image data across all projects that reference the image:
/// the union of requested platforms and the union of extract entries
/// (deduplicated by `path` so the lockfile stays stable).
#[derive(Debug, Default, Clone)]
struct CollectedImage {
    /// Platforms requested for this image (insertion-ordered, deduped).
    platforms: Vec<String>,
    /// Extract entries keyed by `path` so we never write duplicates even
    /// when several projects extract the same binary from the same image.
    extract: BTreeMap<String, LockedOciExtract>,
}

struct LockSyncInputs {
    lockfile_path: PathBuf,
    image_platforms: HashMap<String, CollectedImage>,
    all_platforms: Vec<String>,
    scoped_project_paths: Vec<String>,
    runtimes: Vec<CollectedRuntime>,
    tools: Vec<CollectedTool>,
    tools_platforms: Vec<String>,
    flakes: HashMap<String, String>,
    github_config: Option<GitHubProviderConfig>,
}

struct LockSyncRequest<'a> {
    path: &'a Path,
    options: &'a SyncOptions,
    executor: &'a CommandExecutor,
    scope: LockSyncScope,
}

#[derive(Clone, Copy)]
enum LockSyncScope {
    Path,
    Workspace,
}

#[derive(Clone, Copy)]
enum LockfileSeedMode {
    ResetGeneratedSections,
    PreserveGeneratedSections,
}

fn collect_lock_sync_inputs(
    path: &Path,
    executor: &CommandExecutor,
    scope: LockSyncScope,
) -> Result<LockSyncInputs> {
    let module = match scope {
        LockSyncScope::Path => executor.get_module(path)?,
        LockSyncScope::Workspace => executor.discover_all_modules(path)?,
    };
    let module_root = module.root.clone();
    let lockfile_path = module_root.join(LOCKFILE_NAME);

    let mut image_platforms: HashMap<String, CollectedImage> = HashMap::new();
    let mut all_platforms: Vec<String> = Vec::new();
    let mut scoped_project_paths = Vec::new();
    let mut runtimes = Vec::new();
    let mut tools_map: HashMap<ToolIdentityKey, CollectedTool> = HashMap::new();
    let mut tools_platforms: Vec<String> = Vec::new();
    let mut flakes: HashMap<String, String> = HashMap::new();
    let mut github_config: Option<GitHubProviderConfig> = None;

    for instance in module.projects() {
        scoped_project_paths.push(display_project_path(&instance.path));

        let project: Project = match instance.deserialize() {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to deserialize project, skipping");
                continue;
            }
        };

        match &project.runtime {
            Some(Runtime::Nix(nix_runtime)) => {
                runtimes.push(collect_nix_runtime_lock(
                    &module_root,
                    &instance.path,
                    nix_runtime,
                )?);
            }
            Some(Runtime::Oci(oci_runtime)) => {
                let resolve_platforms = &oci_runtime.platforms;
                if resolve_platforms.is_empty() {
                    return Err(cuenv_core::Error::configuration(format!(
                        "Project '{}' uses OCI runtime but has no platforms configured",
                        project.name
                    )));
                }

                for platform in resolve_platforms {
                    if !all_platforms.contains(platform) {
                        all_platforms.push(platform.clone());
                    }
                }

                for image_spec in &oci_runtime.images {
                    let image = &image_spec.image;
                    let collected = image_platforms.entry(image.clone()).or_default();

                    for platform in resolve_platforms {
                        if !collected.platforms.contains(platform) {
                            collected.platforms.push(platform.clone());
                        }
                    }

                    for extract in &image_spec.extract {
                        collected
                            .extract
                            .entry(extract.path.clone())
                            .or_insert_with(|| LockedOciExtract {
                                path: extract.path.clone(),
                                as_name: extract.as_name.clone(),
                            });
                    }
                }
            }
            Some(Runtime::Tools(tools_runtime)) => {
                let resolve_platforms = &tools_runtime.platforms;
                if resolve_platforms.is_empty() {
                    return Err(cuenv_core::Error::configuration(format!(
                        "Project '{}' uses Tools runtime but has no platforms configured",
                        project.name
                    )));
                }

                for platform in resolve_platforms {
                    if !tools_platforms.contains(platform) {
                        tools_platforms.push(platform.clone());
                    }
                }

                flakes.extend(tools_runtime.flakes.clone());

                if github_config.is_none() {
                    github_config.clone_from(&tools_runtime.github);
                }

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
            Some(_) | None => {}
        }
    }

    Ok(LockSyncInputs {
        lockfile_path,
        image_platforms,
        all_platforms,
        scoped_project_paths,
        runtimes,
        tools: tools_map.into_values().collect(),
        tools_platforms,
        flakes,
        github_config,
    })
}

async fn resolve_oci_artifacts(
    image_platforms: &HashMap<String, CollectedImage>,
) -> Result<Vec<LockedArtifact>> {
    let client = OciClient::new();
    let mut artifacts = Vec::new();

    for (image, collected) in image_platforms {
        debug!(%image, platforms = ?collected.platforms, "Resolving image");

        let mut platforms_map = BTreeMap::new();
        for platform_str in &collected.platforms {
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

        let extract: Vec<LockedOciExtract> = collected.extract.values().cloned().collect();

        artifacts.push(LockedArtifact {
            kind: ArtifactKind::Image {
                image: image.clone(),
                extract,
            },
            platforms: platforms_map,
        });
    }

    Ok(artifacts)
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
async fn execute_lock_sync(request: LockSyncRequest<'_>) -> Result<String> {
    let LockSyncRequest {
        path,
        options,
        executor,
        scope,
    } = request;
    let workspace = matches!(scope, LockSyncScope::Workspace);
    let check = options.mode == SyncMode::Check;
    let inputs = collect_lock_sync_inputs(path, executor, scope)?;
    let LockSyncInputs {
        lockfile_path,
        image_platforms,
        all_platforms,
        scoped_project_paths,
        runtimes: collected_runtimes,
        tools: collected_tools,
        tools_platforms,
        flakes: collected_flakes,
        github_config,
    } = inputs;
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
        let lockfile = seed_lockfile(
            existing_lockfile.as_ref(),
            LockfileSeedMode::ResetGeneratedSections,
        );
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
    let path_local = !workspace;
    let artifacts = resolve_oci_artifacts(&image_platforms).await?;

    // Process Tools runtime
    let seed_mode = if path_local {
        LockfileSeedMode::PreserveGeneratedSections
    } else {
        LockfileSeedMode::ResetGeneratedSections
    };
    let mut lockfile = seed_lockfile(existing_lockfile.as_ref(), seed_mode);

    if path_local {
        for project_path in &scoped_project_paths {
            lockfile.runtimes.remove(project_path);
        }
    }

    for runtime in &collected_runtimes {
        lockfile.upsert_runtime(runtime.project_path.clone(), runtime.runtime.clone())?;
    }

    resolve_tool_locks(
        &mut lockfile,
        ToolLockResolutionRequest {
            tools: &collected_tools,
            platforms: &tools_platforms,
            flakes: collected_flakes,
            existing_lockfile: existing_lockfile.as_ref(),
            options,
            github_token: github_token.as_deref(),
        },
    )
    .await?;

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

fn seed_lockfile(existing: Option<&Lockfile>, mode: LockfileSeedMode) -> Lockfile {
    match mode {
        LockfileSeedMode::PreserveGeneratedSections => {
            existing.cloned().unwrap_or_else(Lockfile::new)
        }
        LockfileSeedMode::ResetGeneratedSections => {
            let mut lockfile = Lockfile::new();
            if let Some(existing) = existing {
                lockfile
                    .tools_activation
                    .clone_from(&existing.tools_activation);
                lockfile.vcs.clone_from(&existing.vcs);
            }
            lockfile
        }
    }
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

#[cfg(test)]
#[path = "lock_tests.rs"]
mod tests;
