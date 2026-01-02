#![allow(missing_docs)]
// Rust 1.92 compiler bug: false positives for thiserror/miette derive macro fields
// https://github.com/rust-lang/rust/issues/147648
#![allow(unused_assignments)]

pub mod affected;
pub mod compiler;
pub mod context;
pub mod diff;
pub mod discovery;
pub mod emitter;
pub mod executor;
pub mod flake;
pub mod gc;
pub mod ir;
pub mod phases;
pub mod pipeline;
pub mod provider;
pub mod render;
pub mod report;

pub use render::StageRenderer;

pub use diff::{DiffError, DigestDiff, compare_by_sha, compare_runs};
pub use gc::{GCConfig, GCError, GCStats, GarbageCollector};

use cuenv_core::Error;
pub type Result<T> = std::result::Result<T, Error>;
