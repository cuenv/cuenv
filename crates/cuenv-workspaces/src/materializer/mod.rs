//! Materialization of dependencies into execution environments.
//!
//! This module handles the "materialization" step where dependencies resolved
//! in the graph are made available to the task execution environment.
//! This typically involves:
//! - Locating the dependency artifacts (in global cache, local node_modules, etc.)
//! - Symlinking or copying them into the hermetic environment
//! - Ensuring workspace members are linked correctly

pub mod cargo_deps;
pub mod node_modules;

use crate::core::types::{LockfileEntry, Workspace};
use crate::error::Result;
use std::path::Path;

/// Trait for materializing dependencies.
pub trait Materializer {
    /// Materialize dependencies into the target directory.
    ///
    /// This should populate `target_dir` (e.g., with a `node_modules` folder
    /// or `target` directory) containing the necessary dependencies.
    fn materialize(
        &self,
        workspace: &Workspace,
        entries: &[LockfileEntry],
        target_dir: &Path,
    ) -> Result<()>;
}
