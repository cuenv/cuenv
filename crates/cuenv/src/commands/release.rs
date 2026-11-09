//! Release management CLI commands.
//!
//! This module provides CLI commands for:
//! - `cuenv changeset add` - Create a new changeset
//! - `cuenv changeset status` - View pending changesets
//! - `cuenv changeset from-commits` - Generate changeset from conventional commits
//! - `cuenv release version` - Calculate and apply version bumps
//! - `cuenv release publish` - Publish workspace packages to crates.io in dependency order

mod binaries;
mod implementation;
mod prepare;

pub use binaries::{ReleaseBinariesOptions, ReleaseBinariesPhase, execute_release_binaries};
#[cfg(test)]
pub use implementation::execute_changeset_status;
pub use implementation::{
    ChangesetDescription, ChangesetStatusOutput, ChangesetSummary, OutputFormat,
    PackageBumpSummary, execute_changeset_add, execute_changeset_from_commits,
    execute_changeset_status_with_format, execute_release_publish, execute_release_version,
};
pub use prepare::{PackageBumpInfo, ReleasePrepareOptions, execute_release_prepare};
