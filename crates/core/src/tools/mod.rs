//! Tool-domain error type.
//!
//! The tool provider runtime (provider trait, registry, activation
//! resolution) lives in the `cuenv-tool-runtime` crate (RFC-0006 phase 3c).
//! Core keeps only [`ToolError`], which `cuenv_core::Error` composes via
//! `#[from]`.

pub mod error;

pub use error::ToolError;
