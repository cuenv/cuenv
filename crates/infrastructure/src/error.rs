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
         The record was saved to {saved_to}; run `cuenv infrastructure state recover` before \
         planning again, or the resource will be created a second time"
    )]
    UnrecordedChange {
        /// Resource address.
        address: String,
        /// Why recording failed.
        reason: String,
        /// Local file holding the unrecorded record.
        saved_to: String,
    },

    /// A provider changed a resource, and neither the state store nor the
    /// local unrecorded-change directory could keep its record. Deliberately
    /// names nothing but the address.
    #[error(
        "{address} was changed by its provider, but its state could not be recorded or saved \
         locally; the resource exists and cuenv no longer tracks it"
    )]
    UnrecordedChangeLost {
        /// Resource address.
        address: String,
    },

    /// Earlier runs left changes that are not yet in the state store.
    #[error(
        "{tenant} has {count} unrecorded change(s) from an earlier run, saved in {directory}; \
         run `cuenv infrastructure state recover` to record them before planning"
    )]
    UnrecordedChangesPending {
        /// Tenant with unrecorded changes.
        tenant: String,
        /// Number of unrecorded records.
        count: usize,
        /// Directory holding them.
        directory: String,
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

    /// Planning was interrupted; nothing was changed or recorded.
    #[error("interrupted while planning; nothing was changed")]
    InterruptedWhilePlanning,

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
    ///
    /// Never pass a `serde_json` error's message into any error: it can
    /// quote the value it failed on. Use [`json_error_category`] instead.
    #[must_use]
    pub fn input_output(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::InputOutput {
            context: context.into(),
            source,
        }
    }
}

/// Name a JSON error's category without its message, which can quote the
/// value (possibly a secret) it failed on.
pub(crate) fn json_error_category(error: &serde_json::Error) -> &'static str {
    match error.classify() {
        serde_json::error::Category::Io => "input or output error",
        serde_json::error::Category::Syntax => "syntax error",
        serde_json::error::Category::Data => "data error",
        serde_json::error::Category::Eof => "unexpected end",
    }
}
