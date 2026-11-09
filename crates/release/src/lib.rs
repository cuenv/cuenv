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
//! ```
//! use cuenv_release::{BumpType, Changeset, ChangesetManager, PackageChange};
//!
//! // Create a new changeset
//! let changeset = Changeset::new(
//!     "Add new feature",
//!     vec![PackageChange::new("cuenv-core", BumpType::Minor)],
//!     Some("This adds a cool new feature".to_string()),
//! );
//!
//! // Store it in a project directory.
//! let project = tempfile::tempdir()?;
//! let manager = ChangesetManager::new(project.path());
//! manager.add(&changeset)?;
//! assert_eq!(manager.list()?.len(), 1);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![warn(missing_docs)]
#![warn(clippy::all, clippy::pedantic)]

pub mod artifact;
pub mod backends;
pub mod changelog;
pub mod changeset;
pub mod commit_analyzer;
pub mod config;
pub mod conventional;
pub mod error;
pub mod manifest;
pub mod orchestrator;
pub mod publish;
pub mod version;

// Re-export main types
pub use artifact::{Artifact, ArtifactBuilder, ChecksumsManifest, PackagedArtifact, Target};
pub use backends::{BackendContext, PublishResult, ReleaseBackend};
pub use changelog::{ChangelogEntry, ChangelogGenerator};
pub use changeset::{BumpType, Changeset, ChangesetManager, PackageChange};
pub use commit_analyzer::{CommitAnalyzer, PackageAffect};
pub use config::{
    ChangelogCategory, ChangelogConfig, CratesBackendConfig, CueBackendConfig, GitHubBackendConfig,
    HomebrewBackendConfig, ReleaseBackendsConfig, ReleaseConfig, ReleaseGitConfig,
    ReleasePackagesConfig, TagType, VersioningStrategy,
};
pub use conventional::{CommitParser, ConventionalCommit, TagSelection};
pub use cuenv_core::DryRun;
pub use error::{Error, Result};
pub use manifest::CargoManifest;
pub use orchestrator::{OrchestratorConfig, ReleaseOrchestrator, ReleasePhase, ReleaseReport};
pub use publish::{PublishPackage, PublishPlan};
pub use version::{Version, VersionCalculator};
