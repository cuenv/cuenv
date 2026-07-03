//! Task execution engine for cuenv (RFC-0006 phase 3a).
//!
//! Owns graph building, scheduling, process management, caching, and
//! command resolution — extracted from `cuenv-core`, which keeps the task
//! DTOs (re-exported from `cuenv-manifest`), `TaskError`, the `AffectedBy`
//! impls, and the pure output-reference parsing shared with module
//! evaluation.
//!
//! The task DTO types (`Task`, `TaskGroup`, `TaskNode`, `Tasks`, ...) are
//! re-exported here for convenience, mirroring the former
//! `cuenv_core::tasks` module surface.

pub mod backend;
pub mod cache;
pub mod captures;
mod command;
mod command_ext;
pub(crate) mod env;
pub mod executor;
pub mod graph;
pub mod graph_walk;
/// Task lookup and resolution across workspaces
pub mod index;
pub mod output_refs;
mod process;
pub mod process_registry;
mod result;
mod shell;
mod workspace;

#[cfg(test)]
pub(crate) mod test_utils;

// Re-export the task DTOs from the leaf manifest crate, mirroring the
// former cuenv_core::tasks surface.
pub use cuenv_manifest::tasks::*;

// The task-domain error stays in cuenv-core (core::Error composes it).
pub use cuenv_core::TaskError;

pub use backend::{
    BackendFactory, HostBackend, TaskBackend, TaskExecutionContext, create_backend,
    create_backend_with_factory, should_use_dagger,
};
pub(crate) use command_ext::TaskCommandExt;
pub use executor::*;
pub use graph::*;
pub use index::{IndexedTask, TaskIndex, TaskPath, WorkspaceTask};
pub use output_refs::{
    OutputRefResolver, TaskOutputField, TaskOutputRef, has_output_refs, process_output_refs,
};
pub use process_registry::global_registry;
pub(crate) use shell::TaskCommandSpec;
