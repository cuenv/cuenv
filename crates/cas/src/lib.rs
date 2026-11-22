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
//! - [`integrity::missing_blobs`] — referential integrity check for an
//!   [`ActionResult`] before it is served as a cache hit.
//!
//! The types in this crate deliberately mirror
//! `build.bazel.remote.execution.v2.*` so that a future remote backend can
//! use the official `bazel-remote-apis` generated types without reshaping
//! any cuenv data structures.

pub mod action_cache;
pub mod cas;
pub mod digest;
pub mod error;
pub mod inspect;
pub mod integrity;
pub mod maintenance;
pub mod merkle;
pub mod message;
pub mod publisher;
pub mod reapi;

pub use action_cache::{ActionCache, CacheSource, LocalActionCache};
pub use cas::{Cas, LocalCas};
pub use digest::{Digest, canonical_bytes, digest_of};
pub use error::{Error, Result};
pub use inspect::{
    ActionDiff, ActionInspection, FieldDiff, InputTreeDiff, InspectedActionResult, InspectedInput,
    MapDiff, SetDiff, compute_action_diff, diff_actions, inspect_action,
};
pub use integrity::{missing_blobs, referenced_blobs};
pub use maintenance::{
    CacheStats, CorruptBlob, DEFAULT_GC_GRACE_WINDOW, GcOptions, GcReport, InvalidActionEntry,
    PruneOptions, RepairReport, VerifyReport,
};
pub use merkle::{
    build_input_tree, build_output_tree, decode_tree, directory_digest, materialize_input_tree,
    materialize_output_tree,
};
pub use message::{
    ACTION_SEMANTICS_VERSION, Action, ActionResult, Command, Directory, DirectoryNode,
    ExecutionMetadata, FileNode, OutputDirectory, OutputFile, Platform, SymlinkNode, Tree,
};
pub use publisher::{ActionPublisher, PublishActionRequest};
pub use reapi::CanonicalMessage;
