//! Pluggable input-file hashing for cuenv.
//!
//! This crate defines [`VcsHasher`], the trait the task executor uses to
//! resolve declared input patterns (globs, directories, explicit paths) and
//! produce a stable SHA-256 per matched file. The default implementation,
//! [`WalkHasher`], walks the filesystem and, inside a git repository,
//! leaves out what the repository's `.gitignore` files ignore.

pub mod error;
pub mod hasher;
mod ignored;
pub mod pattern;
pub mod walker;

pub use error::{Error, Result};
pub use hasher::{HashedInput, VcsHasher};
pub use walker::{Obstacle, Reach, SCRATCH_PREFIX, WalkHasher, WalkOptions, is_scratch_dir};
