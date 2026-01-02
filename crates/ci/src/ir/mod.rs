//! IR v1.5 - Intermediate Representation for CI Pipeline Compiler
//!
//! This module defines the IR schema for cuenv CI pipelines.
//! The IR is the bridge between cuenv task definitions and orchestrator-native CI configurations.
//!
//! ## Version History
//! - v1.5: Unified task model - phase tasks have `phase` field instead of separate `stages`
//! - v1.4: Added `stages` field for provider-injected setup tasks (deprecated in v1.5)
//! - v1.3: Initial stable version

mod schema;
mod validation;

pub use schema::*;
pub use validation::*;
