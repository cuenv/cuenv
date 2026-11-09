//! Release orchestrator.
//!
//! Coordinates the full release pipeline: building, packaging, and publishing
//! to all configured backends.

mod model;
mod package;
mod publish;
mod release_orchestrator;

pub use model::{OrchestratorConfig, ReleasePhase, ReleaseReport};
pub use release_orchestrator::ReleaseOrchestrator;
