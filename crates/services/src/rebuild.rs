//! Running a service's `watch.rebuild` tasks before it is restarted or
//! signalled.
//!
//! The supervisor does not know how to run tasks: `cuenv up` supplies a
//! [`Rebuilder`] that runs them through the task executor.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

/// The outcome of a rebuild: `Err` carries why it failed.
pub type RebuildFuture<'rebuild> =
    Pin<Box<dyn Future<Output = Result<(), RebuildError>> + Send + 'rebuild>>;

/// Why a service's rebuild tasks failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildError(String);

impl RebuildError {
    /// A rebuild failure with the reason shown to the user.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

impl fmt::Display for RebuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RebuildError {}

/// Runs the `watch.rebuild` tasks of a service.
pub trait Rebuilder: Send + Sync {
    /// Run the rebuild tasks of `service`, and their dependencies. A
    /// service without rebuild tasks succeeds at once.
    fn rebuild<'rebuild>(&'rebuild self, service: &'rebuild str) -> RebuildFuture<'rebuild>;
}
