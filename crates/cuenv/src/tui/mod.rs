//! TUI for in-process task execution.
//!
//! [`rich::RichTui`] is launched from `cuenv task --tui` and renders the
//! task graph + per-task output via [`state::TuiState`]. Activity state
//! and UI state live in [`state`]; reusable widgets live in [`widgets`].

pub mod rich;
pub mod state;
pub mod widgets;
