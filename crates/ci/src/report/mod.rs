//! CI Pipeline Reporting
//!
//! Provides types and traits for pipeline execution reporting, including:
//! - Static report types for completed pipelines
//! - Live progress reporting traits
//! - Terminal reporter (provider-specific reporters live in their own crates)

/// Render pipeline reports as JSON.
pub mod json;
/// Render pipeline reports as Markdown.
pub mod markdown;
mod model;
/// Report live pipeline and task progress.
pub mod progress;
/// Display live progress in a terminal.
pub mod terminal;

pub use model::{
    CheckHandle, ContextReport, PipelineReport, PipelineStatus, TaskReport, TaskStatus,
};

// Re-export commonly used types
pub use progress::{
    LivePipelineProgress, LiveTaskProgress, LiveTaskStatus, NoOpReporter, ProgressReporter,
};
pub use terminal::TerminalReporter;
