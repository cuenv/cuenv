//! Root Project configuration type
//!
//! Based on schema/core.cue. All manifest DTOs (including `Project`,
//! services, and hook items) live in `cuenv-manifest`; this module
//! re-exports them at the historical `cuenv_core::manifest` paths during
//! the RFC-0006 migration.

// Re-export the manifest DTOs from the leaf crate so existing
// `cuenv_core::manifest::*` imports keep working during the migration.
pub use cuenv_manifest::manifest::*;

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
