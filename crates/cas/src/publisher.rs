//! Post-run remote action publication contract.
//!
//! Ordinary CAS writes during action construction and local recording stay
//! strictly local so cache hits, failed sandbox setups, failed tasks, and
//! unauthorized runs can never emit remote writes.
//!
//! A configured [`ActionPublisher`] runs **only after** a task has been
//! strictly confined, exited 0, reaped its descendants, projected its outputs
//! cleanly, and recorded its result locally.

use crate::cas::Cas;
use crate::digest::Digest;
use crate::error::Result;
use crate::message::{Action, ActionResult, Command};
use async_trait::async_trait;

/// Everything a remote publisher needs to upload a verified local action.
pub struct PublishActionRequest<'a> {
    /// Local CAS containing the recorded outputs, logs, and `Tree` messages.
    pub local_cas: &'a dyn Cas,
    /// Canonical `Action` message.
    pub action: &'a Action,
    /// Digest of `action`.
    pub action_digest: &'a Digest,
    /// Canonical `Command` message referenced by `action.command_digest`.
    pub command: &'a Command,
    /// Digest of `command`.
    pub command_digest: &'a Digest,
    /// Recorded `ActionResult` to publish last, after all blobs are present.
    pub result: &'a ActionResult,
}

/// Publishes a verified, locally recorded action and all of its referenced
/// blobs to a remote cache.
///
/// Implementations must upload `Action`, `Command`, logs, output files, and
/// every nested `Tree` file blob **before** publishing the `ActionResult`.
#[async_trait]
pub trait ActionPublisher: Send + Sync {
    /// Upload all missing blobs and publish the `ActionResult` last.
    ///
    /// # Errors
    ///
    /// Returns an error if any blob upload or the final `UpdateActionResult`
    /// fails.
    async fn publish(&self, request: PublishActionRequest<'_>) -> Result<()>;
}
