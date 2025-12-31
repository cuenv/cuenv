//! Tool provider system for fetching development tools.
//!
//! This module provides a pluggable system for fetching development tools from
//! various sources (GitHub, OCI, Nix). Each source is implemented
//! as a `ToolProvider` that can resolve and fetch tools.
//!
//! # Architecture
//!
//! The tool system consists of:
//!
//! - [`ToolProvider`] - Trait implemented by each source (GitHub, Nix, etc.)
//! - [`ToolRegistry`] - Collection of registered providers
//! - [`Platform`], [`Os`], [`Arch`] - Platform identification types
//! - [`ToolSource`] - Source-specific resolution data
//! - [`ResolvedTool`] - A fully resolved tool ready to fetch
//! - [`FetchedTool`] - Result of fetching a tool
//!
//! # Example
//!
//! ```ignore
//! use cuenv_core::tools::{ToolRegistry, Platform, ToolOptions};
//!
//! // Create registry with providers
//! let mut registry = ToolRegistry::new();
//! registry.register(GitHubToolProvider::new());
//! registry.register(NixToolProvider::new());
//!
//! // Resolve and fetch a tool
//! let provider = registry.get("github").unwrap();
//! let resolved = provider.resolve("jq", "1.7.1", &Platform::current(), &config).await?;
//! let fetched = provider.fetch(&resolved, &ToolOptions::default()).await?;
//! ```

mod provider;
mod registry;

pub use provider::{
    Arch, FetchedTool, Os, Platform, ResolvedTool, ToolOptions, ToolProvider, ToolSource,
    default_cache_dir,
};
pub use registry::ToolRegistry;
