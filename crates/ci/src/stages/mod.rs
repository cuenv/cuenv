//! Stage configuration for CI pipelines
//!
//! Stage contributors are now defined in CUE (see `contrib/stages/`).
//! This module re-exports IR stage types for use by emitters.

// Re-export stage types from IR for convenience
pub use crate::ir::{BuildStage, StageConfiguration, StageTask};
