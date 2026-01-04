//! Built-in providers for cuenv.
//!
//! This module contains the default providers that ship with cuenv:
//!
//! - [`CiProvider`] - Syncs CI workflow files (GitHub Actions, Buildkite)
//! - [`CodegenProvider`] - Syncs codegen-generated project files
//! - [`RulesProvider`] - Syncs rules configuration (.gitignore, .editorconfig, CODEOWNERS)
//!
//! All of these implement both [`Provider`](crate::Provider) and
//! [`SyncCapability`](crate::SyncCapability).
//!
//! This module also provides detection functions for CI and CODEOWNERS providers:
//!
//! - [`detect_ci_provider`] - Detect the appropriate CI provider
//! - [`detect_code_owners_provider`] - Detect the appropriate CODEOWNERS provider

mod ci;
mod codegen;
mod detection;
mod rules;

pub use ci::CiProvider;
pub use codegen::CodegenProvider;
pub use detection::{detect_ci_provider, detect_code_owners_provider};
pub use rules::RulesProvider;
