//! Base schema discovery across monorepo workspaces
//!
//! This module provides functionality to discover Base configurations across a monorepo,
//! supporting owners and ignore discovery without requiring full Project schemas.

pub mod discovery;

pub use discovery::{BaseDiscovery, BaseEvalFn, DiscoveredBase, DiscoveryError};
