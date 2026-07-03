//! Tool provider runtime for cuenv (RFC-0006 phase 3c).
//!
//! Owns the pluggable tool system extracted from `cuenv-core`: the
//! [`ToolProvider`] trait implemented by each source (GitHub, Nix, OCI,
//! URL, rustup), the [`ToolRegistry`], and activation resolution
//! (lockfile-driven PATH/env planning and application).
//!
//! The tool DTOs ([`Platform`], [`Os`], [`Arch`], [`ToolSource`],
//! [`ToolExtract`], activation steps) live in `cuenv-manifest` and are
//! re-exported here, mirroring the former `cuenv_core::tools` module
//! surface. [`ToolError`] stays in `cuenv-core` (composed into
//! `cuenv_core::Error`) and is re-exported for convenience.

pub mod activation;
mod provider;
mod registry;

pub use activation::{
    ResolvedToolActivationStep, ToolActivationOperation, ToolActivationResolveOptions,
    ToolActivationSource, ToolActivationStep, apply_resolved_tool_activation,
    resolve_tool_activation, validate_tool_activation,
};
pub use cuenv_core::ToolError;
pub use provider::{
    Arch, FetchedTool, Os, Platform, ResolvedTool, ToolExtract, ToolOptions, ToolProvider,
    ToolResolveRequest, ToolSource, default_cache_dir,
};
pub use registry::ToolRegistry;
