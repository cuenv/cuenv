//! Errors raised while talking to a remote cache.

use miette::Diagnostic;
use thiserror::Error;

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// A failure talking to a REAPI server.
#[derive(Debug, Error, Diagnostic)]
#[non_exhaustive]
pub enum Error {
    /// The remote cache configuration is not usable.
    #[error("remote cache configuration: {message}")]
    #[diagnostic(
        code(cuenv::cas_remote::config),
        help("Check the endpoint, instance name and credentials for the remote cache.")
    )]
    Config {
        /// What is wrong.
        message: String,
    },

    /// The connection could not be established.
    #[error("cannot reach remote cache at {endpoint}: {message}")]
    #[diagnostic(
        code(cuenv::cas_remote::connect),
        help("Verify the endpoint is reachable and that grpc:// vs grpcs:// matches the server.")
    )]
    Connect {
        /// Endpoint that was dialled.
        endpoint: String,
        /// Underlying failure.
        message: String,
    },

    /// An RPC failed.
    #[error("remote cache {operation} failed: {message}")]
    #[diagnostic(code(cuenv::cas_remote::rpc))]
    Rpc {
        /// Which call failed, e.g. `FindMissingBlobs`.
        operation: &'static str,
        /// Server-reported failure.
        message: String,
    },

    /// The server is not one cuenv can safely use.
    #[error("remote cache is not compatible: {message}")]
    #[diagnostic(
        code(cuenv::cas_remote::incompatible),
        help("cuenv requires a Bazel Remote Execution API v2 cache using SHA-256 digests.")
    )]
    Incompatible {
        /// What does not line up.
        message: String,
    },

    /// A write was attempted through a read-only client.
    #[error("remote cache is configured read-only: refusing to {operation}")]
    #[diagnostic(
        code(cuenv::cas_remote::read_only),
        help("Set the remote cache mode to read-write to allow uploads.")
    )]
    ReadOnly {
        /// Attempted write.
        operation: &'static str,
    },

    /// The server returned something that does not match what was asked for.
    #[error("remote cache returned an unexpected response for {operation}: {message}")]
    #[diagnostic(code(cuenv::cas_remote::protocol))]
    Protocol {
        /// Which call misbehaved.
        operation: &'static str,
        /// What was wrong with the response.
        message: String,
    },

    /// A conversion between cuenv and REAPI types failed.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Cas(#[from] cuenv_cas::Error),
}

impl Error {
    /// Build a [`Error::Config`].
    #[must_use]
    pub fn config(message: impl Into<String>) -> Self {
        Self::Config {
            message: message.into(),
        }
    }

    /// Build a [`Error::Connect`].
    #[must_use]
    pub fn connect(endpoint: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Connect {
            endpoint: endpoint.into(),
            message: message.into(),
        }
    }

    /// Build a [`Error::Rpc`] from a tonic status.
    #[must_use]
    pub fn rpc(operation: &'static str, status: &tonic::Status) -> Self {
        Self::Rpc {
            operation,
            message: format!("{}: {}", status.code(), status.message()),
        }
    }

    /// Build an [`Error::Incompatible`].
    #[must_use]
    pub fn incompatible(message: impl Into<String>) -> Self {
        Self::Incompatible {
            message: message.into(),
        }
    }

    /// Build an [`Error::Protocol`].
    #[must_use]
    pub fn protocol(operation: &'static str, message: impl Into<String>) -> Self {
        Self::Protocol {
            operation,
            message: message.into(),
        }
    }
}

/// Convert into the `cuenv-cas` error type so remote stores can satisfy the
/// `Cas` and `ActionCache` traits, which are defined in terms of it.
impl From<Error> for cuenv_cas::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Cas(inner) => inner,
            other => Self::serialization(other.to_string()),
        }
    }
}
