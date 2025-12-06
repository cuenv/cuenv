//! Core abstractions for workspace and dependency resolution.
//!
//! This module provides the fundamental traits and types for working with workspaces
//! across different package managers. The architecture is built around:
//!
//! - **Traits** - Define the interface for discovery, parsing, and resolution
//! - **Types** - Provide data structures for representing workspaces and dependencies
//!
//! # Trait-Based Architecture
//!
//! The trait-based design allows for clean separation of concerns and makes it easy
//! to add support for new package managers by implementing the core traits:
//!
//! - [`WorkspaceDiscovery`] - How to find and validate workspace members
//! - [`LockfileParser`] - How to parse package manager-specific lockfiles
//! - [`DependencyResolver`] - How to build dependency graphs from workspace data

pub mod traits;
pub mod types;

// Re-export all public items from submodules
pub use traits::{DependencyResolver, LockfileParser, WorkspaceDiscovery};
pub use types::{
    DependencyRef, DependencySource, DependencySpec, LockfileEntry, PackageManager, Url, Version,
    VersionReq, Workspace, WorkspaceMember,
};
