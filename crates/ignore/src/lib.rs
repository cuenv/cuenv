//! Generate ignore files (.gitignore, .dockerignore, etc.)
//!
//! This crate provides a builder-based API for generating tool-specific ignore files
//! from a declarative configuration.
//!
//! # Example
//!
//! ```rust
//! use cuenv_ignore::{IgnoreFile, IgnoreFiles};
//! # let directory = tempfile::tempdir()?;
//!
//! let result = IgnoreFiles::builder()
//!     .directory(directory.path())
//!     .file(IgnoreFile::new("git")
//!         .pattern("node_modules/")
//!         .pattern(".env"))
//!     .file(IgnoreFile::new("docker")
//!         .pattern("target/"))
//!     .generate()?;
//!
//! for file in &result.files {
//!     println!("{}: {}", file.status, file.filename);
//! }
//! assert!(directory.path().join(".gitignore").exists());
//! assert!(directory.path().join(".dockerignore").exists());
//! # Ok::<(), cuenv_ignore::Error>(())
//! ```
//!
//! # Features
//!
//! - `serde`: Enable serde serialization/deserialization for configuration types

#![warn(missing_docs)]

mod ignore_files;
mod sections;

pub use ignore_files::{
    Error, FileResult, FileStatus, GitRequirement, IgnoreFile, IgnoreFiles, IgnoreFilesBuilder,
    Result, SyncResult, WriteMode,
};
pub(crate) use ignore_files::{determine_file_status, validate_filename};
pub use sections::IgnoreSection;
