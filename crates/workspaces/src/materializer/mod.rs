//! Materialization of dependencies into execution environments.
//!
//! This module handles the "materialization" step where dependencies resolved
//! in the graph are made available to the task execution environment.
//! This typically involves:
//! - Locating the dependency artifacts (in global cache, local `node_modules`, etc.)
//! - Symlinking or copying them into the hermetic environment
//! - Ensuring workspace members are linked correctly

pub mod cargo_deps;
pub mod node_modules;

mod dependency_materializer;

pub use dependency_materializer::DependencyMaterializer;
