//! Sync function implementations.
//!
//! Supports generating:
//! - Project files from CUE codegen templates
//! - CI pipelines from CUE configuration
//!
//! Note: Ignore files and CODEOWNERS are now handled via .rules.cue files.
//! Use `cuenv sync rules` for those.

mod codegen;
mod github;
mod implementation;

pub use codegen::{CodegenSyncOptions, CodegenSyncRequest, execute_sync_codegen};
pub use implementation::{
    CiSyncOptions, CiSyncRequest, CiWorkspaceSyncRequest, execute_sync_ci,
    execute_sync_ci_workspace,
};

#[cfg(test)]
#[path = "functions_tests.rs"]
mod tests;
