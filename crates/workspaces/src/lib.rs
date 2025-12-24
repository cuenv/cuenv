// Transitive dependencies bring in multiple versions of foldhash and unicode-width
#![allow(clippy::multiple_crate_versions)]

//! Workspace and dependency resolution for cuenv across multiple package managers.
//!
//! This crate provides trait-based abstractions for discovering, parsing, and resolving
//! workspace configurations and dependencies across different package managers including
//! npm, Bun, pnpm, Yarn (Classic and Modern), and Cargo.
//!
//! # Architecture
//!
//! The crate is built around three core traits:
//!
//! - [`WorkspaceDiscovery`] - Discovers workspace configuration from a root directory
//! - [`LockfileParser`] - Parses lockfiles into structured entries
//! - [`DependencyResolver`] - Builds dependency graphs from workspace and lockfile data
//!
//! # Workspace Discovery
//!
//! The discovery module provides implementations for finding workspace members from
//! configuration files.
//!
//! ## Feature flags
//!
//! ### Discovery Features
//!
//! - `discovery-javascript` - Enables npm, Bun, Yarn, and pnpm workspace discovery (**enabled by default**)
//! - `discovery-rust` - Enables Cargo workspace discovery
//!
//! ### Fine-grained Discovery Features
//!
//! For minimal dependency footprint:
//! - `discovery-package-json` - npm/Bun/Yarn discovery via `package.json`
//! - `discovery-pnpm` - pnpm discovery via `pnpm-workspace.yaml`
//! - `discovery-cargo` - Cargo discovery via `Cargo.toml`
//!
//! ### Default Features
//!
//! The crate enables the following features by default:
//! - `detection` - Package manager detection from lockfiles and commands
//! - `parsers-javascript` - All JavaScript lockfile parsers
//! - `discovery-javascript` - All JavaScript workspace discoveries
//!
//! This provides a complete out-of-the-box experience for JavaScript/TypeScript projects.
//! To minimize dependencies for Rust-only or specialized use cases, disable default features:
//!
//! ```toml
//! [dependencies]
//! cuenv-workspaces = { version = "...", default-features = false, features = ["discovery-rust"] }
//! ```
//!
//! ## Discovery Behavior for Edge Cases
//!
//! ### Cargo (`CargoTomlDiscovery`)
//!
//! - **Missing `[workspace]` section**: Treated as a valid empty workspace (single-package
//!   repository). Discovery succeeds with zero members.
//! - **Missing or malformed member manifests**: Silently skipped during member enumeration.
//!   Only valid, parseable members are included in the result.
//!
//! ### JavaScript (`PackageJsonDiscovery`, `PnpmWorkspaceDiscovery`)
//!
//! - **Missing or malformed member manifests**: Silently skipped during member enumeration.
//!   Only valid, parseable members with a `name` field are included in the result.
//!
//! This tolerant behavior ensures that discovery does not fail due to individual member
//! issues, allowing partial workspace analysis to proceed.
//!
//! ## Usage examples
//!
//! ```rust,ignore
//! use cuenv_workspaces::{PackageJsonDiscovery, WorkspaceDiscovery};
//! use std::path::Path;
//!
//! let root = Path::new(".");
//! let discovery = PackageJsonDiscovery;
//!
//! if let Ok(workspace) = discovery.discover(root) {
//!     println!("Found workspace with {} members", workspace.member_count());
//! }
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use cuenv_workspaces::{WorkspaceDiscovery, PackageManager, Workspace};
//! use std::path::Path;
//!
//! // Discover a workspace
//! let root = Path::new("/path/to/workspace");
//! let workspace = some_discovery_impl.discover(root)?;
//!
//! // Access workspace information
//! println!("Found {} members", workspace.member_count());
//! for member in &workspace.members {
//!     println!("  - {} at {}", member.name, member.path.display());
//! }
//! ```
//!
//! # Core Types
//!
//! - [`Workspace`] - Represents a discovered workspace with all its members
//! - [`WorkspaceMember`] - Represents a single package/crate in the workspace
//! - [`PackageManager`] - Identifies the package manager in use
//! - [`DependencySpec`] - Describes how a dependency is specified
//! - [`LockfileEntry`] - Represents a resolved dependency from a lockfile
//!
//! # Package Manager Detection
//!
//! The detection module provides automatic package manager detection by scanning
//! for lockfiles and workspace configurations:
//!
//! ```rust,ignore
//! use cuenv_workspaces::detect_package_managers;
//! use std::path::Path;
//!
//! let root = Path::new("/path/to/workspace");
//! let managers = detect_package_managers(root)?;
//!
//! for manager in managers {
//!     println!("Detected: {}", manager);
//! }
//! ```
//!
//! You can also detect package managers from command strings:
//!
//! ```rust,ignore
//! use cuenv_workspaces::detect_from_command;
//!
//! if let Some(manager) = detect_from_command("cargo build") {
//!     println!("Command uses: {}", manager);
//! }
//! ```
//!
//! # Lockfile Parsers
//!
//! The parsers module provides implementations for parsing various package manager
//! lockfiles. Each parser is gated behind a feature flag to minimize binary size.
//!
//! ## Recommended feature flags
//!
//! Use these aggregate features to enable all parsers for an ecosystem:
//!
//! - `parsers-javascript` - Enables all JavaScript parsers (npm, bun, pnpm, yarn classic, yarn modern)
//! - `parsers-rust` - Enables all Rust parsers (currently only Cargo)
//!
//! ## Fine-grained feature flags
//!
//! For minimal dependency footprint, you can enable individual parsers:
//!
//! - `parser-npm` - npm's `package-lock.json` (v3)
//! - `parser-bun` - Bun's `bun.lock` (JSONC format)
//! - `parser-pnpm` - pnpm's `pnpm-lock.yaml`
//! - `parser-yarn-classic` - Yarn Classic (v1.x) `yarn.lock`
//! - `parser-yarn-modern` - Yarn Modern (v2+) `yarn.lock`
//! - `parser-cargo` - Cargo's `Cargo.lock`
//!
//! ## Usage examples
//!
//! ```rust,ignore
//! use cuenv_workspaces::{NpmLockfileParser, LockfileParser};
//! use std::path::Path;
//!
//! let parser = NpmLockfileParser;
//! let entries = parser.parse(Path::new("package-lock.json"))?;
//!
//! for entry in entries {
//!     println!("{} @ {}", entry.name, entry.version);
//! }
//! ```
//!
//! ```rust,ignore
//! use cuenv_workspaces::{CargoLockfileParser, LockfileParser};
//! use std::path::Path;
//!
//! let parser = CargoLockfileParser;
//! let entries = parser.parse(Path::new("Cargo.lock"))?;
//!
//! for entry in entries {
//!     println!("{} @ {}", entry.name, entry.version);
//! }
//! ```

#![warn(missing_docs)]
#![warn(clippy::all, clippy::pedantic)]

pub mod core;
pub mod error;
pub mod materializer;
pub mod resolver;

#[cfg(feature = "detection")]
pub mod detection;

#[cfg(any(
    feature = "parsers-javascript",
    feature = "parsers-rust",
    feature = "parser-cargo"
))]
pub mod parsers;

#[cfg(any(
    feature = "discovery-package-json",
    feature = "discovery-pnpm",
    feature = "discovery-cargo"
))]
pub mod discovery;

// Re-export core types
pub use core::{
    DependencyRef, DependencySource, DependencySpec, LockfileEntry, PackageManager, Version,
    VersionReq, Workspace, WorkspaceMember,
};

// Re-export traits
pub use core::{DependencyResolver, LockfileParser, WorkspaceDiscovery};

// Re-export resolver types
pub use resolver::GenericDependencyResolver;

// Re-export error types
pub use error::{Error, Result};

// Re-export detection functions
#[cfg(feature = "detection")]
pub use detection::{detect_from_command, detect_package_managers, detect_with_command_hint};

// Re-export JavaScript discovery types
#[cfg(feature = "discovery-package-json")]
pub use discovery::PackageJsonDiscovery;

#[cfg(feature = "discovery-pnpm")]
pub use discovery::PnpmWorkspaceDiscovery;

// Re-export Rust discovery types
#[cfg(feature = "discovery-cargo")]
pub use discovery::CargoTomlDiscovery;

// Re-export JavaScript parser types
#[cfg(feature = "parser-bun")]
pub use parsers::javascript::BunLockfileParser;
#[cfg(feature = "parser-npm")]
pub use parsers::javascript::NpmLockfileParser;
#[cfg(feature = "parser-pnpm")]
pub use parsers::javascript::PnpmLockfileParser;
#[cfg(feature = "parser-yarn-classic")]
pub use parsers::javascript::YarnClassicLockfileParser;
#[cfg(feature = "parser-yarn-modern")]
pub use parsers::javascript::YarnModernLockfileParser;
// Re-export Rust parser types
#[cfg(any(feature = "parsers-rust", feature = "parser-cargo"))]
pub use parsers::rust::CargoLockfileParser;
