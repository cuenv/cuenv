//! CI Pipeline Execution Module
//!
//! Provides three modalities for CI pipeline management:
//!
//! 1. **Unified Runner** (`cuenv ci --pipeline <name>`)
//!    - Runs pipelines locally or in CI environments identically
//!    - Local parallel process execution with bounded concurrency
//!    - Reports to GitHub Check Runs with live updates
//!
//! 2. **Dynamic Export** (`cuenv ci --pipeline <name> --export <format>`)
//!    - Generates pipeline YAML to stdout or file
//!    - Supports: buildkite, gitlab, github-actions, circleci
//!
//! 3. **Static Sync** (via `cuenv sync ci`)
//!    - Generates workflow files with thin/expanded modes
//!    - Configurable per-pipeline in env.cue

mod args;
mod exporter;
mod runner;

pub use args::{CiArgs, ExportFormat};

use cuenv_core::Result;

/// Execute CI command based on arguments.
///
/// Routes to either:
/// - Export mode: Generate pipeline YAML
/// - Runner mode: Execute pipeline locally/in CI
///
/// # Errors
///
/// Returns error if pipeline execution fails, export fails, or configuration is invalid.
pub async fn execute_ci(args: CiArgs) -> Result<()> {
    // Export mode: generate pipeline YAML
    if let Some(format) = args.export {
        return exporter::execute_export(&args, format).await;
    }

    // Runner mode: execute pipeline
    runner::execute_runner(&args).await
}
