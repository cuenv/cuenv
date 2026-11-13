//! Compile and execute CI pipelines from evaluated cuenv projects.

/// Select tasks affected by changed files and their dependencies.
pub mod affected;
/// Lower project tasks and contributors into CI intermediate representation.
pub mod compiler;
/// What a project's CUE configuration is built from.
pub mod configuration;
/// Describe the CI event and source revision that triggered a run.
pub mod context;
/// Compare reports from two CI runs and explain cache invalidation.
pub mod diff;
/// Discover and evaluate projects in a CUE module for CI.
pub mod discovery;
/// Emit pipeline intermediate representation for an orchestrator.
pub mod emitter;
/// Errors produced by CI compilation, emission, execution, and maintenance.
pub mod error;
/// Execute compiled CI pipelines and collect their results.
pub mod executor;
/// Analyze flake lock files and runtime purity for reproducible CI.
pub mod flake;
/// Prune unused local cache and Nix store data.
pub mod gc;
/// Pipeline intermediate representation and validation.
pub mod ir;
/// Pipeline phase types shared with emitters.
pub mod phases;
/// Resolve pipeline task references and matrix expansions.
pub mod pipeline;
/// CI provider interface and local provider implementation.
pub mod provider;
/// Pipeline reports and live progress reporting.
pub mod report;

pub use diff::{DiffError, DigestDiff, compare_by_sha, compare_runs};
pub use gc::{GCConfig, GCError, GCStats, GarbageCollector};

pub use error::Result;
