//! Content-addressed store and action cache for cuenv.
//!
//! This crate provides the caching primitives used by the task executor:
//!
//! - [`Digest`] — `(sha256, size)` pair identifying a blob.
//! - [`Cas`] + [`LocalCas`] — a blob store.
//! - [`ActionCache`] + [`LocalActionCache`] — maps [`Action`] digests to
//!   [`ActionResult`] records.
//! - [`message`] — Bazel Remote Execution v2-shaped messages
//!   ([`Action`], [`Command`], [`Directory`], [`ActionResult`], …).
//! - [`merkle::build_input_tree`] / [`merkle::materialize_input_tree`] —
//!   Merkle-tree construction and materialization.
//!
//! The types in this crate deliberately mirror
//! `build.bazel.remote.execution.v2.*` so that a future remote backend can
//! use the official `bazel-remote-apis` generated types without reshaping
//! any cuenv data structures.

pub mod action_cache;
pub mod cas;
pub mod digest;
pub mod error;
pub mod merkle;
pub mod message;

pub use action_cache::{ActionCache, LocalActionCache};
pub use cas::{Cas, LocalCas};
pub use digest::{Digest, canonical_bytes, digest_of};
pub use error::{Error, Result};
pub use merkle::{build_input_tree, directory_digest, materialize_input_tree};
pub use message::{
    Action, ActionResult, Command, Directory, DirectoryNode, ExecutionMetadata, FileNode,
    OutputDirectory, OutputFile, Platform, SymlinkNode,
};
