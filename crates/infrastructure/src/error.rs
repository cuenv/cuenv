//! Error type for the infrastructure engine.

use thiserror::Error;

/// Result alias for this crate.
pub type Result<Success> = std::result::Result<Success, InfrastructureError>;

/// Errors raised while planning or applying infrastructure.
#[derive(Debug, Error)]
pub enum InfrastructureError {
    /// Invalid `infrastructure` configuration.
    #[error("infrastructure configuration error: {0}")]
    Configuration(String),

    /// Failure encoding or decoding Terraform values.
    #[error("value codec error: {0}")]
    Codec(String),

    /// Failure launching or talking to a provider plugin process.
    #[error("provider plugin error: {0}")]
    Plugin(String),

    /// A gRPC call to a provider failed.
    #[error("provider procedure {method} failed: {status}")]
    RemoteProcedure {
        /// Procedure path.
        method: String,
        /// gRPC status returned by the provider.
        status: Box<tonic::Status>,
    },

    /// A provider returned error diagnostics.
    #[error("{context}:\n{}", .errors.join("\n"))]
    Diagnostics {
        /// What cuenv was doing when the provider failed.
        context: String,
        /// Rendered error diagnostics.
        errors: Vec<String>,
    },

    /// Failure downloading or installing a provider from a registry.
    #[error("provider install error: {0}")]
    Install(String),

    /// State storage failure.
    #[error("state store error: {0}")]
    State(String),

    /// The tenant's state is locked by another run.
    #[error(
        "state for {tenant} is locked by '{holder}' (lock {lock_identifier}, acquired {acquired_at}); \
         if that run is gone, release it with `cuenv infrastructure unlock {lock_identifier}`"
    )]
    Locked {
        /// Tenant whose state is locked.
        tenant: String,
        /// Lock identifier.
        lock_identifier: String,
        /// Description of the lock holder.
        holder: String,
        /// When the lock was acquired.
        acquired_at: String,
    },

    /// The run no longer holds the tenant's lock, so it must not write.
    #[error(
        "state for {tenant} is no longer locked by this run (lock {lock_identifier}); \
         another run may have taken over, so nothing further was written"
    )]
    LockLost {
        /// Tenant whose lock was lost.
        tenant: String,
        /// The lock identifier this run held.
        lock_identifier: String,
    },

    /// A provider changed a resource but its new state could not be recorded.
    #[error(
        "{address} was changed by its provider, but recording its state failed: {reason}. \
         The new state was saved to {saved_to}; record it before applying again, or the \
         resource will be created a second time"
    )]
    UnrecordedChange {
        /// Resource address.
        address: String,
        /// Why recording failed.
        reason: String,
        /// Local file holding the unrecorded state.
        saved_to: String,
    },

    /// The run was interrupted between resources.
    #[error(
        "interrupted after applying {completed} of {total} changes; every applied change is recorded"
    )]
    Interrupted {
        /// Changes applied and recorded before stopping.
        completed: usize,
        /// Changes the plan contained.
        total: usize,
    },

    /// Input or output failure.
    #[error("{context}: {source}")]
    InputOutput {
        /// What cuenv was doing.
        context: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

impl InfrastructureError {
    /// Build a configuration error.
    #[must_use]
    pub fn configuration(message: impl Into<String>) -> Self {
        Self::Configuration(message.into())
    }

    /// Build a codec error.
    #[must_use]
    pub fn codec(message: impl Into<String>) -> Self {
        Self::Codec(message.into())
    }

    /// Build a plugin error.
    #[must_use]
    pub fn plugin(message: impl Into<String>) -> Self {
        Self::Plugin(message.into())
    }

    /// Build an install error.
    #[must_use]
    pub fn install(message: impl Into<String>) -> Self {
        Self::Install(message.into())
    }

    /// Build a state store error.
    #[must_use]
    pub fn state(message: impl Into<String>) -> Self {
        Self::State(message.into())
    }

    /// Build an input or output error with context.
    #[must_use]
    pub fn input_output(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::InputOutput {
            context: context.into(),
            source,
        }
    }
}
