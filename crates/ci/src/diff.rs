//! Digest Diff Tool
//!
//! Compares two CI runs to identify what caused cache invalidation.
//! Shows changed files, environment variables, and upstream outputs
//! without exposing secret values.

mod comparison;
mod format;

pub use crate::error::DiffError;
pub use comparison::{
    ChangeType, DiffSummary, DigestDiff, TaskDiff, compare_by_sha, compare_reports, compare_runs,
};
pub use format::format_diff;
