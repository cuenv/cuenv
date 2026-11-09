//! Generate CODEOWNERS files with configurable formatting.
//!
//! This crate provides a builder-based API for generating CODEOWNERS files
//! that define code ownership rules for your repository. It is provider-agnostic;
//! platform-specific logic (paths, section styles) belongs in provider crates
//! like `cuenv-github` or `cuenv-gitlab`.
//!
//! # Example
//!
//! ```rust
//! use cuenv_codeowners::{CodeOwners, SectionStyle, Rule};
//!
//! let codeowners = CodeOwners::builder()
//!     .section_style(SectionStyle::Comment)  // GitHub/Bitbucket style
//!     .rule(Rule::new("*", ["@org/core-team"]))
//!     .rule(Rule::new("*.rs", ["@rust-team"]))
//!     .rule(Rule::new("/docs/**", ["@docs-team"]).section("Documentation"))
//!     .build();
//!
//! let content = codeowners.generate();
//! ```
//!
//! # Section Styles
//!
//! - `Comment`: `# Section Name` (used by GitHub, Bitbucket)
//! - `Bracket`: `[Section Name]` (used by GitLab)
//! - `None`: No section headers
//!
//! # Provider Support
//!
//! The [`provider`] module provides a trait-based abstraction for syncing
//! CODEOWNERS files. Provider implementations live in platform crates.
//!
//! # Features
//!
//! - `serde`: Enable serde serialization/deserialization for all types
//! - `schemars`: Enable JSON Schema generation (implies `serde`)

#![warn(missing_docs)]

mod codeowners;
pub mod provider;

pub use codeowners::{CodeOwners, CodeOwnersBuilder, DEFAULT_CUENV_HEADER, Rule, SectionStyle};
