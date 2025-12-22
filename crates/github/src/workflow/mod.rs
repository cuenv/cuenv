//! GitHub Actions Workflow Generator
//!
//! Generates static GitHub Actions workflow files from cuenv's intermediate
//! representation (IR). Unlike Buildkite's dynamic pipelines, GitHub Actions
//! requires committed workflow files in `.github/workflows/`.
//!
//! # Example
//!
//! ```ignore
//! use cuenv_github::workflow::GitHubActionsEmitter;
//! use cuenv_ci::emitter::Emitter;
//!
//! let emitter = GitHubActionsEmitter::new()
//!     .with_runner("ubuntu-latest")
//!     .with_nix()
//!     .with_cachix("my-cache");
//!
//! // Single workflow output (implements Emitter trait)
//! let yaml = emitter.emit(&ir)?;
//!
//! // Multi-workflow output for projects with multiple pipelines
//! let workflows = emitter.emit_workflows(&ir)?;
//! for (filename, content) in workflows {
//!     std::fs::write(format!(".github/workflows/{}", filename), content)?;
//! }
//! ```

pub mod emitter;
pub mod schema;

pub use emitter::{GitHubActionsEmitter, ReleaseWorkflowBuilder};
pub use schema::*;
