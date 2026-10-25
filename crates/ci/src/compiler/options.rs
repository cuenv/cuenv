//! [`CompilerOptions`]: how the [`Compiler`](super::Compiler) lowers a project.

use crate::ir::{CachePolicy, PurityMode};
use cuenv_core::ci::Pipeline;
use std::collections::HashMap;
use std::path::PathBuf;

/// Compiler configuration options
#[derive(Clone, Default)]
pub struct CompilerOptions {
    /// Default purity mode for runtimes
    pub purity_mode: PurityMode,

    /// Whether to validate inputs exist at compile time
    pub validate_inputs: bool,

    /// Default cache policy for tasks
    pub default_cache_policy: CachePolicy,

    /// Path to flake.lock file (optional, auto-detected if not set)
    pub flake_lock_path: Option<PathBuf>,

    /// Project root directory (for locating flake.lock)
    pub project_root: Option<PathBuf>,

    /// Manual overrides for input digests (for Override mode)
    /// Maps input name to override digest value
    pub input_overrides: HashMap<String, String>,

    /// Pipeline name (for environment-aware compilation)
    ///
    /// When set, this is used for workflow naming and identification.
    pub pipeline_name: Option<String>,

    /// Pipeline being compiled (for environment-aware compilation)
    ///
    /// When set, the compiler will set `ir.pipeline.environment` from
    /// the pipeline's environment, enabling CUE stage contributors to
    /// evaluate their activation conditions.
    pub pipeline: Option<Pipeline>,

    /// Enable CI mode for orchestrator artifact handling.
    ///
    /// When true:
    /// - Task outputs use `OutputType::Orchestrator` for cross-job artifact sharing
    /// - Task input references (`inputs: [{task: "..."}]`) are converted to `artifact_downloads`
    pub ci_mode: bool,

    /// Module root (repo root / cue.mod location)
    ///
    /// Used for constructing trigger paths relative to the repository root.
    pub module_root: Option<PathBuf>,

    /// Project path relative to module root
    ///
    /// Used as a fallback for trigger paths when tasks have no explicit inputs.
    /// For example, if a project is at `projects/rawkode.academy/api`, this would
    /// be `"projects/rawkode.academy/api"`.
    pub project_path: Option<String>,
}

impl std::fmt::Debug for CompilerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompilerOptions")
            .field("purity_mode", &self.purity_mode)
            .field("validate_inputs", &self.validate_inputs)
            .field("default_cache_policy", &self.default_cache_policy)
            .field("flake_lock_path", &self.flake_lock_path)
            .field("project_root", &self.project_root)
            .field("input_overrides", &self.input_overrides)
            .field("pipeline_name", &self.pipeline_name)
            .field("pipeline", &self.pipeline)
            .field("ci_mode", &self.ci_mode)
            .field("module_root", &self.module_root)
            .field("project_path", &self.project_path)
            .finish()
    }
}
