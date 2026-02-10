//! Git hook utilities for finding the repository root.
//!
//! This module provides:
//!
//! - [`find_git_root`] - Find the repository root using gix

use cuenv_core::Result;
use std::path::{Path, PathBuf};

/// Find the git repository root directory.
///
/// Uses gix to discover the repository from the given path.
///
/// # Errors
///
/// Returns an error if not in a git repository.
pub fn find_git_root(start_path: &Path) -> Result<PathBuf> {
    let repo = gix::discover(start_path)
        .map_err(|e| cuenv_core::Error::configuration(format!("Not in a git repository: {e}")))?;

    // Get the working directory (workdir) which is the repository root
    let workdir = repo
        .workdir()
        .ok_or_else(|| cuenv_core::Error::configuration("Cannot operate in a bare repository"))?;

    Ok(workdir.to_path_buf())
}
