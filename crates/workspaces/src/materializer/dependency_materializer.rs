//! Dependency materialization contract.

use crate::core::types::{LockfileEntry, Workspace};
use crate::error::Result;
use std::path::Path;

/// Trait for materializing dependencies.
pub trait DependencyMaterializer {
    /// Materialize dependencies into the target directory.
    ///
    /// This should populate `target_dir` (e.g., with a `node_modules` folder
    /// or `target` directory) containing the necessary dependencies.
    ///
    /// # Errors
    ///
    /// Returns an error if dependencies cannot be linked or copied into the
    /// target directory (for example, due to missing files or filesystem
    /// permission issues).
    fn materialize(
        &self,
        workspace: &Workspace,
        entries: &[LockfileEntry],
        target_dir: &Path,
    ) -> Result<()>;
}

/// Link a directory using the platform's directory symlink operation.
#[cfg(any(unix, windows))]
pub(super) fn symlink_directory(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, destination)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(source, destination)
    }
}
