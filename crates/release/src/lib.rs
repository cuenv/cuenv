//! Native release management for cuenv.
//!
//! This crate provides a comprehensive release management subsystem within cuenv,
//! handling versioning, changelogs, and publishing. It replaces external tools like
//! `changesets` or `sampo` by integrating these workflows directly into cuenv's
//! hermetic environment and task graph.
//!
//! # Features
//!
//! - **Changeset Workflow**: Centralized changeset storage in `.cuenv/changesets/`
//! - **Monorepo Awareness**: Leverages cuenv's workspace graph for dependency propagation
//! - **Version Calculation**: Semantic versioning with fixed and linked package groups
//! - **Changelog Generation**: Automated changelog updates per package and workspace
//! - **Topological Publishing**: Publishes packages in dependency order
//!
//! # Architecture
//!
//! The crate is organized around several core modules:
//!
//! - [`changeset`] - Changeset creation, parsing, and storage
//! - [`version`] - Version calculation and bumping logic
//! - [`changelog`] - Changelog generation and formatting
//! - [`config`] - Release configuration types
//! - [`publish`] - Publishing workflow and topological ordering
//!
//! # Example
//!
//! ```rust,ignore
//! use cuenv_release::{Changeset, ChangesetManager, BumpType};
//! use std::path::Path;
//!
//! // Create a new changeset
//! let changeset = Changeset::new(
//!     "Add new feature",
//!     vec![("cuenv-core".to_string(), BumpType::Minor)],
//!     Some("This adds a cool new feature".to_string()),
//! );
//!
//! // Store it
//! let manager = ChangesetManager::new(Path::new("."));
//! manager.add(&changeset)?;
//! ```

#![warn(missing_docs)]
#![warn(clippy::all, clippy::pedantic)]

pub mod changelog;
pub mod changeset;
pub mod config;
pub mod conventional;
pub mod error;
pub mod manifest;
pub mod publish;
pub mod version;

// Re-export main types
pub use changelog::{ChangelogEntry, ChangelogGenerator};
pub use changeset::{BumpType, Changeset, ChangesetManager, PackageChange};
pub use config::{ChangelogConfig, ReleaseConfig, ReleaseGitConfig, ReleasePackagesConfig};
pub use conventional::{CommitParser, ConventionalCommit};
pub use error::{Error, Result};
pub use manifest::CargoManifest;
pub use publish::PublishPlan;
pub use version::{Version, VersionCalculator};
