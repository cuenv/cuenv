//! Immutable CUE instance snapshots and fresh runtime specializations.
//!
//! A snapshot owns source bytes and the loaded import graph, never a Go handle.
//! Every call creates a fresh CUE context. Loading, inspection and validation
//! wait at most the configured [`crate::evaluation_timeout`] per operation.
//! Admission waits within that same deadline before starting a worker. A
//! timed-out worker can keep running, retaining one of 16 process-wide frozen
//! evaluation slots until it returns. Specialization remains synchronous and
//! shares those slots, leaving deadlines and cancellation to its caller.
//! No call can interrupt CUE or guarantee a hard evaluation-time or memory bound.
//! Dropping a snapshot releases its Rust-owned bytes; Go temporaries become
//! eligible for garbage collection after each actual operation returns.

mod prepared;
mod snapshot;

pub use prepared::{
    PreparationOptions, PreparedInstance, PreparedSpecializationOptions, VerificationOptions,
};
pub use snapshot::{
    DeferredValue, FreezeOptions, FrozenDeclaration, FrozenEvaluation, FrozenEvaluationOptions,
    FrozenInstance, FrozenReference, FrozenValidationOptions, ScalarFill, ScalarValue,
    SpecializationOptions, SpecializationResult, ValuePath, ValueSelector,
};
