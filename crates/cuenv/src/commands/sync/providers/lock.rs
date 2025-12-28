//! OCI lockfile sync provider.
//!
//! Resolves OCI image references to content-addressed digests and writes
//! them to `cuenv.lock` for hermetic binary resolution.

use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_core::lockfile::{ArtifactKind, LockedArtifact, Lockfile, PlatformData, LOCKFILE_NAME};
use cuenv_core::manifest::{Base, Project, Runtime};
use std::collections::HashMap;
use std::path::Path;

use crate::commands::CommandExecutor;
use crate::commands::sync::provider::{SyncMode, SyncOptions, SyncProvider, SyncResult};

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
        let output = execute_lock_sync(path, package, check, executor)?;
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
        let output = execute_lock_sync(Path::new("."), package, check, executor)?;
        Ok(SyncResult::success(output))
    }
}

/// Execute lock synchronization for a path.
///
/// Scans all projects in the CUE module, collects OCI image references,
/// resolves them to digests, and writes `cuenv.lock`.
fn execute_lock_sync(
    path: &Path,
    _package: &str,
    check: bool,
    executor: &CommandExecutor,
) -> Result<String> {
    // Get the module evaluation
    let module = executor.get_module(path)?;

    let module_root = module.root.clone();
    let lockfile_path = module_root.join(LOCKFILE_NAME);

    // Collect all OCI artifacts from projects
    let mut artifacts: Vec<LockedArtifact> = Vec::new();
    let mut seen_image: HashMap<String, usize> = HashMap::new();
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

            if let Some(&idx) = seen_image.get(image) {
                // Already have this artifact, merge platforms if needed
                for platform in resolve_platforms {
                    if !artifacts[idx].platforms.contains_key(platform) {
                        // Placeholder digest - real resolution happens in oci-provider crate
                        artifacts[idx].platforms.insert(
                            platform.clone(),
                            PlatformData {
                                digest: format!("sha256:pending-{}", platform),
                                size: None,
                            },
                        );
                    }
                }
            } else {
                // New artifact
                let mut platforms_map = HashMap::new();
                for platform in resolve_platforms {
                    // Placeholder digest - real resolution happens in oci-provider crate
                    platforms_map.insert(
                        platform.clone(),
                        PlatformData {
                            digest: format!("sha256:pending-{}", platform),
                            size: None,
                        },
                    );
                }

                let idx = artifacts.len();
                artifacts.push(LockedArtifact {
                    kind: ArtifactKind::Image {
                        image: image.clone(),
                    },
                    platforms: platforms_map,
                });
                seen_image.insert(image.clone(), idx);
            }
        }
    }

    if artifacts.is_empty() {
        return Ok("No OCI artifacts found in any project.".to_string());
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

        let artifact_count = lockfile.artifacts.len();
        let platform_str = all_platforms.join(", ");
        Ok(format!(
            "Wrote {} artifacts for platform(s) [{}] to {}",
            artifact_count,
            platform_str,
            lockfile_path.display()
        ))
    }
}
