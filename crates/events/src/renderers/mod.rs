//! Event renderers for different output formats.
//!
//! Renderers consume events and produce output to various destinations.

pub mod cli;
pub mod json;
pub mod recorder;
pub mod spinner;

pub use cli::CliRenderer;
pub use json::JsonRenderer;
pub use recorder::{EventRecorder, EventReplayReader, RecorderError};
pub use spinner::SpinnerRenderer;
