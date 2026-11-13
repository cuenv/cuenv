//! CI provider trait and implementations.
//!
//! This module provides a trait-based abstraction for CI/CD providers.
//!
//! # Provider Implementations
//!
//! - [`LocalProvider`]: Fallback provider for local development (included in this crate)
//! - `cuenv-github`: [`GitHubCIProvider`](https://docs.rs/cuenv-github) for GitHub Actions
//!
//! # Example
//!
//! ```
//! use cuenv_ci::provider::CIProvider;
//! use cuenv_ci::provider::local::LocalProvider;
//!
//! // Try to detect a CI provider, fall back to local
//! let provider = LocalProvider::detect().expect("LocalProvider always detects");
//! let runtime = tokio::runtime::Runtime::new()?;
//! let changes = runtime.block_on(provider.changed_files())?;
//! assert_eq!(provider.context().provider, "local");
//! # let _ = changes;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod changed_files;
mod ci_provider;
/// The git checkout a provider runs in, read in process.
pub mod git;
/// Fallback provider for running CI pipelines locally.
pub mod local;

pub use local::LocalProvider;

pub use changed_files::{ChangedFiles, NoBase};
pub use ci_provider::CIProvider;
