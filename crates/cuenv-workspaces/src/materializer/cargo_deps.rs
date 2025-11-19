//! Materializer for Cargo dependencies.

use super::Materializer;
use crate::core::types::{LockfileEntry, PackageManager, Workspace};
use crate::error::{Error, Result};
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(windows)]
use std::os::windows::fs::symlink_dir as symlink;

/// Materializer for Cargo projects.
pub struct CargoMaterializer;

impl Materializer for CargoMaterializer {
    fn materialize(
        &self,
        workspace: &Workspace,
        _entries: &[LockfileEntry],
        target_dir: &Path,
    ) -> Result<()> {
        if workspace.manager != PackageManager::Cargo {
            return Ok(());
        }

        // Symlink the target directory to share build artifacts
        // This allows reusing incremental compilation results
        let workspace_target = workspace.root.join("target");
        let env_target = target_dir.join("target");

        if !workspace_target.exists() {
            std::fs::create_dir_all(&workspace_target).map_err(|e| Error::Io {
                source: e,
                path: Some(workspace_target.clone()),
                operation: "create workspace target directory".to_string(),
            })?;
        }

        if env_target.exists() {
            // If it exists (e.g. from previous run or created by inputs), remove it
            // to allow symlinking the shared target directory.
            if env_target.is_symlink() || env_target.is_file() {
                std::fs::remove_file(&env_target).map_err(|e| Error::Io {
                    source: e,
                    path: Some(env_target.clone()),
                    operation: "removing existing target symlink/file".to_string(),
                })?;
            } else {
                std::fs::remove_dir_all(&env_target).map_err(|e| Error::Io {
                    source: e,
                    path: Some(env_target.clone()),
                    operation: "removing existing target directory".to_string(),
                })?;
            }
        }

        // We assume target_dir is the root of the hermetic environment.
        // Cargo expects 'target' at the root usually.

        // Note: Symlinking 'target' might cause locking issues if multiple tasks run in parallel
        // and try to write to the same shared target.
        // However, Cargo handles concurrent builds relatively well with file locking.
        // But different tasks might need different profiles or features.
        // Ideally, we should use CARGO_TARGET_DIR env var instead of symlinking,
        // but symlinking works if we want it to appear local.

        if let Err(e) = symlink(&workspace_target, &env_target) {
            return Err(Error::Io {
                source: e,
                path: Some(env_target),
                operation: "symlink target dir".to_string(),
            });
        }

        Ok(())
    }
}
