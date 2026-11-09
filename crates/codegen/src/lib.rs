//! # cuenv-codegen
//!
//! CUE-powered code generation and project scaffolding from CUE templates.
//!
//! This crate provides a code generation system that:
//! - Uses schema-wrapped code blocks (e.g., `schema.#TypeScript`, `schema.#JSON`)
//! - Supports managed (always regenerated) and scaffold (generate once) file modes
//! - Integrates with `cuenv sync codegen` command
//!
//! ## How it works
//!
//! A Codegen configuration is a CUE-based template that defines multiple
//! files to generate for a project. Define your files in CUE with type-safe
//! schemas, then sync them with `cuenv sync codegen`.
//!
//! ## Example
//!
//! ```cue
//! schema.#Project & {
//!     name: "my-service"
//!     codegen: {
//!         files: {
//!             "package.json": schema.#JSON & {
//!                 mode: "managed"
//!                 content: """{"name": "my-service"}"""
//!             }
//!         }
//!     }
//! }
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]
#![warn(clippy::pedantic)]

pub mod codegen;
pub mod config;
pub mod formatter;
pub mod generator;

pub use codegen::Codegen;
pub use generator::{GenerateOptions, Generator};

mod error;

pub use error::{CodegenError, Result};
