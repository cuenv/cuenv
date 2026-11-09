//! GitHub workflow sync operations.

mod implementation;
pub(super) mod infrastructure;
mod matrix;

#[cfg(test)]
pub(super) use implementation::has_matrix_tasks;
pub(super) use implementation::{GithubSyncRequest, execute_sync_github};
