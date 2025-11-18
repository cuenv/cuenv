//! Materializer for Cargo dependencies.

use super::Materializer;
use crate::core::types::{LockfileEntry, Workspace, PackageManager};
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

        if workspace_target.exists() {
            if env_target.exists() {
                 // If it exists (e.g. from previous run or created by inputs), remove it?
                 // Or assumes it's empty.
                 // fs::remove_dir_all(&env_target)?; 
            }
            
            // We assume target_dir is the root of the hermetic environment.
            // Cargo expects 'target' at the root usually.
            
            // Note: Symlinking 'target' might cause locking issues if multiple tasks run in parallel
            // and try to write to the same shared target.
            // However, Cargo handles concurrent builds relatively well with file locking.
            // But different tasks might need different profiles or features.
            // Ideally, we should use CARGO_TARGET_DIR env var instead of symlinking,
            // but symlinking works if we want it to appear local.
            
            match symlink(&workspace_target, &env_target) {
                Ok(_) => {},
                Err(e) => {
                    // Ignore if already exists or other harmless errors?
                    // For now, fail if we can't link.
                    // But if env_target already exists (as a dir), we can't link.
                     return Err(Error::Io {
                        source: e,
                        path: Some(env_target),
                        operation: "symlink target dir".to_string(),
                    });
                }
            }
        }

        Ok(())
    }
}
