//! Event renderers for different output formats.
//!
//! Renderers consume events and produce output to various destinations.

pub mod cli;
pub mod json;

pub use cli::CliRenderer;
pub use json::JsonRenderer;
