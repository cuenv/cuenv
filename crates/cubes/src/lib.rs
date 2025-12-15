//! # cuenv-cubes
//!
//! CUE Cubes - code generation and project scaffolding from CUE templates.
//!
//! This crate provides a code generation system based on CUE Cubes that:
//! - Uses schema-wrapped code blocks (e.g., `schema.#TypeScript`, `schema.#JSON`)
//! - Supports managed (always regenerated) and scaffold (generate once) file modes
//! - Integrates with `cuenv sync cubes` command
//!
//! ## What is a CUE Cube?
//!
//! A Cube is a CUE-based template that generates multiple project files.
//! Define your files in CUE with type-safe schemas, then sync them with
//! `cuenv sync cubes`.
//!
//! ## Example
//!
//! ```cue
//! schema.#Project & {
//!     name: "my-service"
//!     cube: {
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

pub mod config;
pub mod cube;
pub mod formatter;
pub mod generator;

pub use cube::Cube;
pub use generator::{GenerateOptions, Generator};

use thiserror::Error;

/// Errors that can occur during code generation
#[derive(Error, Debug)]
pub enum CodegenError {
    /// Error loading or evaluating CUE Cube
    #[error("Cube error: {0}")]
    Cube(String),

    /// Error during file generation
    #[error("Generation error: {0}")]
    Generation(String),

    /// Error during formatting
    #[error("Formatting error: {0}")]
    Formatting(String),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON error
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Result type for codegen operations
pub type Result<T> = std::result::Result<T, CodegenError>;
