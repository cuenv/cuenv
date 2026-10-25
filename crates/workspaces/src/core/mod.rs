//! Core abstractions for workspace discovery and lockfile parsing.
//!
//! This module provides the fundamental traits and types for working with workspaces
//! across different package managers. The architecture is built around:
//!
//! - **Traits** - Define the interface for discovery and parsing
//! - **Types** - Provide data structures for representing workspaces and dependencies
//!
//! # Trait-Based Architecture
//!
//! The trait-based design allows for clean separation of concerns and makes it easy
//! to add support for new package managers by implementing the core traits:
//!
//! - [`WorkspaceDiscovery`] - How to find and validate workspace members
//! - [`LockfileParser`] - How to parse package manager-specific lockfiles

pub mod traits;
pub mod types;

// Re-export all public items from submodules
pub use traits::{LockfileParser, WorkspaceDiscovery};
pub use types::{
    DependencyRef, DependencySource, DependencySpec, LockfileEntry, PackageManager, Url, Version,
    VersionReq, Workspace, WorkspaceMember,
};
