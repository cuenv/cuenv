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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_workspace(root: &Path, manager: PackageManager) -> Workspace {
        Workspace::new(root.to_path_buf(), manager)
    }

    #[test]
    fn test_cargo_materializer_skips_non_cargo() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = make_workspace(temp_dir.path(), PackageManager::Npm);
        let target_dir = temp_dir.path().join("hermetic");
        fs::create_dir_all(&target_dir).unwrap();

        let materializer = CargoMaterializer;
        let result = materializer.materialize(&workspace, &[], &target_dir);

        assert!(result.is_ok());
        // No symlink should be created
        assert!(!target_dir.join("target").exists());
    }

    #[test]
    fn test_cargo_materializer_creates_target_dir() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = make_workspace(temp_dir.path(), PackageManager::Cargo);
        let target_dir = temp_dir.path().join("hermetic");
        fs::create_dir_all(&target_dir).unwrap();

        let materializer = CargoMaterializer;
        let result = materializer.materialize(&workspace, &[], &target_dir);

        assert!(result.is_ok());
        // Workspace target should be created
        assert!(temp_dir.path().join("target").exists());
        // Hermetic env should have a symlink
        assert!(target_dir.join("target").exists());
    }

    #[test]
    fn test_cargo_materializer_replaces_existing_symlink() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = make_workspace(temp_dir.path(), PackageManager::Cargo);
        let target_dir = temp_dir.path().join("hermetic");
        fs::create_dir_all(&target_dir).unwrap();

        // Create an existing symlink pointing somewhere else
        let other_dir = temp_dir.path().join("other");
        fs::create_dir_all(&other_dir).unwrap();
        let env_target = target_dir.join("target");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&other_dir, &env_target).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&other_dir, &env_target).unwrap();

        let materializer = CargoMaterializer;
        let result = materializer.materialize(&workspace, &[], &target_dir);

        assert!(result.is_ok());
        // Symlink should now point to workspace target
        let workspace_target = temp_dir.path().join("target");
        assert!(target_dir.join("target").exists());
        assert!(workspace_target.exists());
    }

    #[test]
    fn test_cargo_materializer_replaces_existing_directory() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = make_workspace(temp_dir.path(), PackageManager::Cargo);
        let target_dir = temp_dir.path().join("hermetic");
        fs::create_dir_all(&target_dir).unwrap();

        // Create an existing directory with some content
        let env_target = target_dir.join("target");
        fs::create_dir_all(&env_target).unwrap();
        fs::write(env_target.join("somefile"), "content").unwrap();

        let materializer = CargoMaterializer;
        let result = materializer.materialize(&workspace, &[], &target_dir);

        assert!(result.is_ok());
        // Directory should be replaced with symlink
        assert!(target_dir.join("target").exists());
    }

    #[test]
    fn test_cargo_materializer_with_existing_workspace_target() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = make_workspace(temp_dir.path(), PackageManager::Cargo);
        let target_dir = temp_dir.path().join("hermetic");
        fs::create_dir_all(&target_dir).unwrap();

        // Pre-create workspace target with content
        let workspace_target = temp_dir.path().join("target");
        fs::create_dir_all(&workspace_target).unwrap();
        fs::write(workspace_target.join("marker"), "exists").unwrap();

        let materializer = CargoMaterializer;
        let result = materializer.materialize(&workspace, &[], &target_dir);

        assert!(result.is_ok());
        // Symlink should point to existing workspace target
        assert!(target_dir.join("target").exists());
        // Original content should be preserved
        assert!(workspace_target.join("marker").exists());
    }
}
