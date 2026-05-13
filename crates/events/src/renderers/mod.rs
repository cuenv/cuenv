//! Event renderers for different output formats.
//!
//! Renderers consume events and produce user-facing output (terminal
//! drawing, JSON streams, etc.). Bus sinks that don't render — like the
//! event recorder — live in [`crate::subscribers`] instead.

pub mod cli;
pub mod json;
#[cfg(feature = "spinner")]
pub mod spinner;

pub use cli::CliRenderer;
pub use json::JsonRenderer;
#[cfg(feature = "spinner")]
pub use spinner::SpinnerRenderer;
