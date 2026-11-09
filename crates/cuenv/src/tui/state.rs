//! Centralized state management for the rich TUI.
//!
//! State is split into two concerns:
//!
//! - **Activity model** — the event-driven data: tasks, outputs, running
//!   set, completion status. Mutated **only** by [`TuiState::apply_event`]
//!   (and helpers it calls). Two replays of the same event trace produce
//!   identical activity state; the integration tests in
//!   `tests/tui_replay.rs` exercise that invariant.
//! - **UI state** — the input-driven view state: cursor position,
//!   expansion, focus, selected task, scroll offset. Mutated only by
//!   keyboard handlers.
//!
//! [`TuiState`] coordinates those halves and preserves the renderer-facing
//! read APIs while keeping mutation boundaries explicit.

mod activity;
mod tree;
mod tui_state;
mod view;

pub use activity::{ActivityModel, OutputLine, TaskInfo, TaskOutput, TaskStatus};
pub use tui_state::{CommandOutcome, TuiState};
pub use view::{OutputMode, TreeNodeType, TreeViewItem, UiState};
