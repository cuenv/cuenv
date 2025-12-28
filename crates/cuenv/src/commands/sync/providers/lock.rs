//! OCI lockfile sync provider.
//!
//! Resolves OCI image references to content-addressed digests and writes
//! them to `cuenv.lock` for hermetic binary resolution.
//!
//! For Homebrew images, this also resolves transitive dependencies and
//! includes them in the lockfile.

use async_trait::async_trait;
use cuenv_core::lockfile::{ArtifactKind, LockedArtifact, Lockfile, PlatformData, LOCKFILE_NAME};
use cuenv_core::manifest::{Base, Project, Runtime};
use cuenv_core::Result;
use cuenv_oci_provider::{
    formula_name_from_image, is_homebrew_image, resolve_with_deps, to_homebrew_platform,
    HomebrewFormula, OciClient, Platform,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tracing::{debug, info, warn};

use crate::commands::sync::provider::{SyncMode, SyncOptions, SyncProvider, SyncResult};
use crate::commands::CommandExecutor;

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

/// Execute lock synchronization for a path.
///
/// Scans all projects in the CUE module, collects OCI image references,
/// resolves them to digests, and writes `cuenv.lock`.
///
/// For Homebrew images, this also fetches formula metadata and resolves
/// transitive dependencies, adding them to the lockfile.
async fn execute_lock_sync(
    path: &Path,
    _package: &str,
    check: bool,
    executor: &CommandExecutor,
) -> Result<String> {
    // Collect all OCI artifacts from projects (image -> platforms needed)
    // Note: We collect all data before async operations to avoid holding
    // the module guard across await points (MutexGuard is not Send).
    let (lockfile_path, image_platforms, all_platforms) = {
        let module = executor.get_module(path)?;
        let module_root = module.root.clone();
        let lockfile_path = module_root.join(LOCKFILE_NAME);

        let mut image_platforms: HashMap<String, Vec<String>> = HashMap::new();
        let mut all_platforms: Vec<String> = Vec::new();

        for instance in module.projects() {
            // Deserialize the instance to get the Project struct
            let project: Project = match instance.deserialize() {
                Ok(p) => p,
                Err(_) => continue,
            };

            // Check if project uses OCI runtime
            let oci_runtime = match &project.runtime {
                Some(Runtime::Oci(oci)) => oci,
                _ => continue,
            };

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

        (lockfile_path, image_platforms, all_platforms)
    };
    // Module guard is now dropped, we can safely use async operations

    if image_platforms.is_empty() {
        return Ok("No OCI artifacts found in any project.".to_string());
    }

    // Separate Homebrew images from regular images
    let (homebrew_images, regular_images): (Vec<_>, Vec<_>) = image_platforms
        .iter()
        .partition(|(image, _)| is_homebrew_image(image));

    info!(
        "Resolving {} Homebrew formulas + {} regular images for {} platforms",
        homebrew_images.len(),
        regular_images.len(),
        all_platforms.len()
    );

    let client = OciClient::new();
    let mut artifacts: Vec<LockedArtifact> = Vec::new();

    // Process Homebrew images with dependency resolution
    let mut resolved_formulas: HashSet<String> = HashSet::new();
    for (image, platforms) in &homebrew_images {
        let formula_name = formula_name_from_image(image).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "Failed to extract formula name from '{}'",
                image
            ))
        })?;

        // Skip if already resolved as a dependency
        if resolved_formulas.contains(&formula_name) {
            continue;
        }

        // Resolve formula with all transitive dependencies
        info!(%formula_name, "Resolving Homebrew formula with dependencies");
        let formulas = resolve_with_deps(&formula_name).await.map_err(|e| {
            cuenv_core::Error::configuration(format!(
                "Failed to resolve Homebrew formula '{}': {}",
                formula_name, e
            ))
        })?;

        debug!(
            formula = %formula_name,
            deps = ?formulas.iter().map(|f| &f.name).collect::<Vec<_>>(),
            "Resolved dependency tree"
        );

        // Create artifacts for each formula (including dependencies)
        for formula in formulas {
            if resolved_formulas.contains(&formula.name) {
                continue;
            }

            let artifact =
                resolve_homebrew_formula(&client, &formula, platforms, &all_platforms).await?;
            artifacts.push(artifact);
            resolved_formulas.insert(formula.name.clone());
        }
    }

    // Process regular (non-Homebrew) images
    for (image, platforms) in &regular_images {
        debug!(%image, ?platforms, "Resolving image");

        let mut platforms_map = HashMap::new();
        for platform_str in *platforms {
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

    // Create lockfile
    let mut lockfile = Lockfile::new();
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

        let homebrew_count = lockfile
            .artifacts
            .iter()
            .filter(|a| matches!(a.kind, ArtifactKind::Homebrew { .. }))
            .count();
        let image_count = lockfile.artifacts.len() - homebrew_count;
        let platform_str = all_platforms.join(", ");

        Ok(format!(
            "Wrote {} Homebrew formulas + {} images for platform(s) [{}] to {}",
            homebrew_count,
            image_count,
            platform_str,
            lockfile_path.display()
        ))
    }
}

/// Resolve a Homebrew formula to a lockfile artifact.
///
/// Uses the bottle SHA256 from formula metadata as the digest.
async fn resolve_homebrew_formula(
    _client: &OciClient,
    formula: &HomebrewFormula,
    requested_platforms: &[String],
    _all_platforms: &[String],
) -> Result<LockedArtifact> {
    debug!(
        name = %formula.name,
        version = %formula.versions.stable,
        deps = ?formula.dependencies,
        "Resolving Homebrew bottle"
    );

    let mut platforms_map = HashMap::new();

    for platform_str in requested_platforms {
        // Convert cuenv platform to Homebrew platform
        let homebrew_platform = match to_homebrew_platform(platform_str) {
            Some(p) => p,
            None => {
                warn!(
                    %platform_str,
                    formula = %formula.name,
                    "Platform not supported by Homebrew, skipping"
                );
                continue;
            }
        };

        // Get bottle info from formula metadata
        let bottle_file = formula.get_bottle(&homebrew_platform).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "Homebrew formula '{}' has no bottle for platform '{}' (Homebrew: '{}')",
                formula.name, platform_str, homebrew_platform
            ))
        })?;

        // The bottle URL is the OCI reference
        // ghcr.io/v2/homebrew/core/<name>/blobs/sha256:<digest>
        // We use the sha256 from the bottle file as the digest
        let digest = format!("sha256:{}", bottle_file.sha256);

        debug!(
            formula = %formula.name,
            %platform_str,
            %digest,
            "Resolved bottle digest"
        );

        platforms_map.insert(
            platform_str.clone(),
            PlatformData {
                digest,
                size: None,
            },
        );
    }

    Ok(LockedArtifact {
        kind: ArtifactKind::Homebrew {
            name: formula.name.clone(),
            version: formula.versions.stable.clone(),
            dependencies: formula.dependencies.clone(),
        },
        platforms: platforms_map,
    })
}
