//! VCS dependency sync provider.

mod git;
mod materialization;
mod partial;
mod paths;
mod vcs_sync_provider;

pub use vcs_sync_provider::VcsSyncProvider;
