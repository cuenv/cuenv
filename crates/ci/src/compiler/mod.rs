//! Compiler from cuenv task definitions to IR v1.5
//!
//! Transforms a cuenv `Project` with tasks into an intermediate representation
//! suitable for emitting orchestrator-native CI configurations.
//!
//! ## Contributors
//!
//! Contributors are defined in CUE (see `contrib/contributors/`). The compiler
//! evaluates the `ci.contributors` array and injects active contributors'
//! tasks into the appropriate build phases.
//!
//! ## Layout
//!
//! Each compiler submodule extends the same type with its own `impl Compiler` block:
//! `purity` (flake analysis), `lowering` (task nodes to IR tasks),
//! `contributors` and `triggers`. [`CompilerOptions`] lives in `options`.

mod ci_compiler;
mod contributors;
/// Compute stable digests for task and runtime inputs.
pub mod digest;
mod lowering;
mod options;
mod purity;
mod triggers;

pub use crate::error::CompilerError;
pub use ci_compiler::Compiler;
pub use options::{CompilerOptions, FileAnchors};
pub use purity::RuntimeSpec;

#[cfg(test)]
#[path = "compiler_tests.rs"]
mod tests;
