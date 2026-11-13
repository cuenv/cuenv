//! The [`VcsHasher`] trait.

use crate::error::Result;
use crate::walker::WalkOptions;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A single input file resolved and hashed by a [`VcsHasher`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HashedInput {
    /// Path relative to the workspace root, with forward slashes.
    pub relative_path: PathBuf,
    /// Absolute path on disk (canonicalized when possible).
    pub absolute_path: PathBuf,
    /// Lowercase hex SHA-256 of the file content.
    pub sha256: String,
    /// File size in bytes.
    pub size: u64,
    /// Whether the file is executable when materialized.
    pub is_executable: bool,
}

/// A pluggable strategy for resolving glob patterns and hashing the matched
/// files.
///
/// The baseline implementation is [`walker::WalkHasher`](crate::walker::WalkHasher),
/// which walks the filesystem and streams SHA-256 over each matched file.
/// Future implementations can use a VCS (e.g. git index lookups) to skip
/// re-hashing files whose content hasn't changed.
#[async_trait]
pub trait VcsHasher: Send + Sync {
    /// Resolve `patterns` (globs, directories, or explicit file paths) and
    /// return a [`HashedInput`] for every matched file.
    ///
    /// Results are deduplicated and returned in deterministic order so the
    /// same inputs always produce the same sequence.
    ///
    /// # Errors
    ///
    /// Returns an error if a pattern is invalid or if any filesystem
    /// operation fails.
    async fn resolve_and_hash(&self, patterns: &[String]) -> Result<Vec<HashedInput>>;

    /// Resolve and hash `patterns` as [`Self::resolve_and_hash`] does, with
    /// a walk that `options` may confine, such as to the real directories
    /// of the repository for the paths of a file reference.
    ///
    /// # Errors
    ///
    /// As [`Self::resolve_and_hash`], and when a pattern cannot be resolved
    /// within what `options` allows.
    async fn resolve_and_hash_with(
        &self,
        patterns: &[String],
        options: WalkOptions,
    ) -> Result<Vec<HashedInput>>;

    /// Short, stable identifier for this implementation (e.g. `"walk"`,
    /// `"git"`). Useful for diagnostics and metrics.
    fn name(&self) -> &'static str;
}
