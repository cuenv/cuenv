//! Pluggable input-file hashing for cuenv.
//!
//! This crate defines [`VcsHasher`], the trait the task executor uses to
//! resolve declared input patterns (globs, directories, explicit paths) and
//! produce a stable SHA-256 per matched file. The default implementation,
//! [`WalkHasher`], walks the filesystem; VCS-aware implementations (git and
//! friends) can plug in later behind the same trait so the executor
//! doesn't care how the hashes were obtained.

pub mod error;
pub mod hasher;
pub mod walker;

pub use error::{Error, Result};
pub use hasher::{HashedInput, VcsHasher};
pub use walker::WalkHasher;
