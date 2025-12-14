//! # cuenv-codegen
//!
//! Literate project scaffolding and management using CUE templates.
//!
//! This crate provides a code generation system based on pure CUE blueprints that:
//! - Uses schema-wrapped code blocks (e.g., `code.#TypeScript`, `code.#Rust`)
//! - Supports managed (always regenerated) and scaffold (generate once) file modes
//! - Auto-generates formatter configs from CUE schemas
//! - Provides formatting integration for multiple languages
//!
//! ## Architecture
//!
//! - `blueprint`: Load and evaluate CUE blueprints
//! - `generator`: Core file generation logic
//! - `formatter`: Language-specific formatting
//! - `config`: Generate formatter configs (biome.json, .prettierrc, etc.)
//!
//! ## Example
//!
//! ```ignore
//! use cuenv_codegen::{Blueprint, Generator};
//!
//! let blueprint = Blueprint::load("blueprint.cue")?;
//! let generator = Generator::new(blueprint);
//! generator.generate()?;
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]
#![warn(clippy::pedantic)]

pub mod blueprint;
pub mod config;
pub mod formatter;
pub mod generator;

pub use blueprint::Blueprint;
pub use generator::Generator;

use thiserror::Error;

/// Errors that can occur during code generation
#[derive(Error, Debug)]
pub enum CodegenError {
    /// Error loading or evaluating CUE blueprint
    #[error("Blueprint error: {0}")]
    Blueprint(String),

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
