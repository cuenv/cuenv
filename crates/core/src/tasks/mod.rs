//! Task DTO surface and shared task logic.
//!
//! The task DTO types (`Task`, `TaskGroup`, `TaskNode`, `Tasks`, ...) live
//! in `cuenv-manifest` and are re-exported here. The execution engine
//! (graph building, scheduling, process management, caching, command
//! resolution) lives in the `cuenv-task-exec` crate (RFC-0006 phase 3a).
//! This module keeps what core itself needs: [`TaskError`] (composed into
//! `cuenv_core::Error`), the pure output-reference parsing shared with
//! module evaluation, and the `AffectedBy` impls for the task DTOs (which
//! cannot move — the trait and the types are both foreign to the engine
//! crate).

pub mod error;
pub mod file_references;
pub mod output_refs;

// Re-export the task DTOs from the leaf manifest crate.
pub use cuenv_manifest::tasks::*;

pub use error::{TaskError, TaskFailureOutput, TaskFailureWithHelp};

pub use output_refs::{TaskOutputField, TaskOutputRef, has_output_refs, process_output_refs};

mod affected;

#[cfg(test)]
#[path = "tasks_tests.rs"]
mod tests;
