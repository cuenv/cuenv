//! IR v1.3 - Intermediate Representation for CI Pipeline Compiler
//!
//! This module defines the IR schema for cuenv CI pipelines as specified in PRD v1.3.
//! The IR is the bridge between cuenv task definitions and orchestrator-native CI configurations.

mod schema;
mod validation;

pub use schema::*;
pub use validation::*;
