//! CLI renderer for cuenv events.
//!
//! Renders events to stdout/stderr for terminal display.

mod ci;
mod cli_renderer;
mod command;
mod interactive;
mod output;
mod service;
mod system;
mod task;

pub use cli_renderer::{CliRenderer, CliRendererConfig, CliSpinnerConfig};
