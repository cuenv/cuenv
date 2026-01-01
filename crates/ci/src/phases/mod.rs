//! Phase configuration for CI pipelines
//!
//! Contributors are now defined in CUE (see `contrib/contributors/`).
//! This module re-exports IR phase types for use by emitters.

// Re-export phase types from IR for convenience
pub use crate::ir::{BuildStage, StageConfiguration, StageTask};
