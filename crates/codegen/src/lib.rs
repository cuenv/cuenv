//! # cuenv-codegen
//!
//! Literate project scaffolding and management using CUE Cubes.
//!
//! This crate provides a code generation system based on pure CUE Cubes that:
//! - Uses schema-wrapped code blocks (e.g., `code.#TypeScript`, `code.#Rust`)
//! - Supports managed (always regenerated) and scaffold (generate once) file modes
//! - Auto-generates formatter configs from CUE schemas
//! - Provides formatting integration for multiple languages
//!
//! ## What is a CUE Cube?
//!
//! A Cube is a CUE-based template that generates multiple project files.
//! Think of it as a 3D blueprint - each face of the cube represents
//! different aspects of your project (source code, config, tests, etc.)
//!
//! ## Architecture
//!
//! - `cube`: Load and evaluate CUE Cubes
//! - `generator`: Core file generation logic
//! - `formatter`: Language-specific formatting
//! - `config`: Generate formatter configs (biome.json, .prettierrc, etc.)
//!
//! ## Example
//!
//! ```ignore
//! use cuenv_codegen::{Cube, Generator};
//!
//! let cube = Cube::load("my-project.cube.cue")?;
//! let generator = Generator::new(cube);
//! generator.generate()?;
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
