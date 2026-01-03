//! Content-addressed task caching for cuenv
//!
//! This crate provides the caching infrastructure for cuenv task execution:
//! - Content-addressed storage based on input file hashes
//! - Deterministic cache key computation
//! - Task result serialization and materialization
//! - Workspace snapshot archiving
//!
//! # Overview
//!
//! The cache system enables fast task re-execution by storing:
//! - Task outputs indexed by a deterministic cache key
//! - Execution metadata (duration, exit code, environment)
//! - Workspace snapshots for hermetic execution
//!
//! # Cache Key Computation
//!
//! Cache keys are computed from:
//! - Input file content hashes
//! - Command and arguments
//! - Environment variables
//! - cuenv version and platform

// TODO(cache-docs): Add # Errors documentation to all fallible public functions
#![expect(
    clippy::missing_errors_doc,
    reason = "Error documentation to be added incrementally"
)]

mod error;
pub mod tasks;

// Re-export error types at crate root
pub use error::{Error, Result};

// Re-export main types
pub use tasks::{
    CacheEntry, CacheKeyEnvelope, OutputIndexEntry, TaskLatestIndex, TaskLogs, TaskResultMeta,
    compute_cache_key, get_project_cache_keys, key_to_path, lookup, lookup_latest,
    materialize_outputs, record_latest, save_result, snapshot_workspace_tar_zst,
};
