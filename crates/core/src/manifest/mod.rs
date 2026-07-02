//! Root Project configuration type
//!
//! Based on schema/core.cue. The task-independent DTOs live in
//! `cuenv-manifest` and are re-exported here; `Project`, services, and hook
//! items stay in this crate until the task types migrate too (RFC-0006).

// Re-export the manifest DTOs from the leaf crate so existing
// `cuenv_core::manifest::*` imports keep working during the migration.
pub use cuenv_manifest::manifest::*;

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
