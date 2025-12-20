//! Buildkite CI Pipeline Emitter for cuenv
//!
//! This crate provides a Buildkite pipeline emitter that transforms cuenv's
//! intermediate representation (IR) into Buildkite pipeline YAML format.
//!
//! # Example
//!
//! ```ignore
//! use cuenv_buildkite::BuildkiteEmitter;
//! use cuenv_ci::emitter::Emitter;
//! use cuenv_ci::ir::IntermediateRepresentation;
//!
//! let emitter = BuildkiteEmitter::new()
//!     .with_emojis()
//!     .with_default_queue("linux-x86");
//!
//! let ir: IntermediateRepresentation = /* ... */;
//! let yaml = emitter.emit(&ir)?;
//!
//! println!("{}", yaml);
//! ```
//!
//! # IR to Buildkite Mapping
//!
//! | IR Field | Buildkite YAML |
//! |----------|----------------|
//! | `task.id` | `key` |
//! | `task.command` | `command` |
//! | `task.env` | `env` |
//! | `task.secrets` | `env` (variable references) |
//! | `task.depends_on` | `depends_on` |
//! | `task.resources.tags` | `agents: { queue: "tag" }` |
//! | `task.concurrency_group` | `concurrency_group` + `concurrency: 1` |
//! | `task.manual_approval` | `block` step before task |
//! | `task.outputs` (orchestrator) | `artifact_paths` |

pub mod emitter;
pub mod provider;
pub mod schema;

pub use emitter::BuildkiteEmitter;
pub use provider::BuildkiteCIProvider;
