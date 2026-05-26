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

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use cuenv_events::{CuenvEvent, EventCategory, Stream, TaskEvent};

mod activity;
mod tree;
mod view;

pub use activity::{ActivityModel, OutputLine, TaskInfo, TaskOutput, TaskStatus};
pub use view::{OutputMode, TreeNodeType, TreeViewItem, UiState};

#[cfg(test)]
use activity::MAX_OUTPUT_LINES;

/// Global TUI state for task execution.
///
/// Composed of two halves with structurally-enforced boundaries:
/// - [`ActivityModel`] — event-driven, deterministic when replayed.
/// - [`UiState`] — input-driven, survives event floods.
///
/// Both halves expose read-only accessors; mutations go through methods
/// on [`TuiState`] (or [`Self::apply_event`] for activity-model writes).
#[derive(Debug)]
pub struct TuiState {
    model: ActivityModel,
    ui: UiState,
}

impl TuiState {
    /// Create a new TUI state
    #[must_use]
    pub fn new() -> Self {
        Self {
            model: ActivityModel::new(),
            ui: UiState::new(),
        }
    }

    /// Borrow the activity model (read-only).
    #[must_use]
    pub const fn model(&self) -> &ActivityModel {
        &self.model
    }

    /// Borrow the UI state (read-only).
    #[must_use]
    pub const fn ui(&self) -> &UiState {
        &self.ui
    }

    // ---- Forwarding read accessors --------------------------------------
    // Convenience for the rich TUI / widget callers that historically read
    // these fields directly. They delegate to the underlying half so the
    // invariants stay encapsulated.

    /// Tasks indexed by name. See [`ActivityModel::tasks`].
    #[must_use]
    pub const fn tasks(&self) -> &HashMap<String, TaskInfo> {
        self.model.tasks()
    }

    /// Outputs indexed by task name. See [`ActivityModel::outputs`].
    #[must_use]
    pub const fn outputs(&self) -> &HashMap<String, TaskOutput> {
        self.model.outputs()
    }

    /// Currently running task names. See [`ActivityModel::running_tasks`].
    #[must_use]
    pub fn running_tasks(&self) -> &[String] {
        self.model.running_tasks()
    }

    /// See [`ActivityModel::is_complete`].
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.model.is_complete()
    }

    /// See [`ActivityModel::success`].
    #[must_use]
    pub const fn success(&self) -> bool {
        self.model.success()
    }

    /// See [`ActivityModel::error_message`].
    #[must_use]
    pub fn error_message(&self) -> Option<&str> {
        self.model.error_message()
    }

    /// Currently selected task. See [`UiState::selected_task`].
    #[must_use]
    pub fn selected_task(&self) -> Option<&str> {
        self.ui.selected_task()
    }

    /// Expanded tree nodes. See [`UiState::expanded_nodes`].
    #[must_use]
    pub const fn expanded_nodes(&self) -> &HashSet<String> {
        self.ui.expanded_nodes()
    }

    /// Cursor position. See [`UiState::cursor_position`].
    #[must_use]
    pub const fn cursor_position(&self) -> usize {
        self.ui.cursor_position()
    }

    /// Flattened tree view. See [`UiState::flattened_tree`].
    #[must_use]
    pub fn flattened_tree(&self) -> &[TreeViewItem] {
        self.ui.flattened_tree()
    }

    /// Output panel mode. See [`UiState::output_mode`].
    #[must_use]
    pub const fn output_mode(&self) -> OutputMode {
        self.ui.output_mode()
    }

    /// Output panel scroll offset. See [`UiState::output_scroll`].
    #[must_use]
    pub const fn output_scroll(&self) -> usize {
        self.ui.output_scroll()
    }

    /// Focused task in expanded-output mode. See [`UiState::focused_task`].
    #[must_use]
    pub fn focused_task(&self) -> Option<&str> {
        self.ui.focused_task()
    }

    /// Toggle focus on the highlighted task — when focused, the renderer
    /// hides the task tree and gives the entire viewport to that task's
    /// output panel. Calling this with the same task again clears focus.
    pub fn toggle_focus(&mut self) {
        if self.ui.focused_task.is_some() {
            self.ui.focused_task = None;
            return;
        }
        let Some(node) = self.highlighted_node() else {
            return;
        };
        if let TreeNodeType::Task(name) = &node.node_type {
            let name = name.clone();
            self.ui.focused_task = Some(name.clone());
            self.ui.selected_task = Some(name);
            self.ui.output_mode = OutputMode::Selected;
            self.ui.output_scroll = 0;
        }
    }

    /// Exit focused-task mode (no-op when not focused).
    pub fn clear_focus(&mut self) {
        self.ui.focused_task = None;
    }

    /// Scroll the output panel by `delta` lines. Negative scrolls up
    /// (saturating at 0); positive scrolls down. Replaces the previous
    /// pub-field write pattern so the invariant stays in one place.
    pub fn scroll_output_by(&mut self, delta: isize) {
        let magnitude = delta.unsigned_abs();
        if delta >= 0 {
            self.ui.output_scroll = self.ui.output_scroll.saturating_add(magnitude);
        } else {
            self.ui.output_scroll = self.ui.output_scroll.saturating_sub(magnitude);
        }
    }

    /// Add a task to the state
    pub fn add_task(&mut self, task: TaskInfo) {
        let name = task.name.clone();
        self.model.tasks.insert(name.clone(), task);
        self.model
            .outputs
            .insert(name.clone(), TaskOutput::new(name));
    }

    /// Update a task's status.
    ///
    /// Once a task reaches a terminal status (`Completed`, `Failed`, `Cached`,
    /// `Skipped`), further status writes are ignored so late or duplicate
    /// events — common during replay or under broadcast lag — can't regress
    /// a finished task back to `Running` or `Pending`.
    pub fn update_task_status(&mut self, name: &str, status: TaskStatus) {
        let Some(task) = self.model.tasks.get_mut(name) else {
            tracing::warn!(
                "Attempted to update status for unknown task '{}' to {:?}",
                name,
                status
            );
            return;
        };

        if Self::is_terminal_status(task.status) {
            return;
        }

        task.status = status;
        match status {
            TaskStatus::Running => {
                task.start_time = Some(Instant::now());
                if !self.model.running_tasks.contains(&name.to_string()) {
                    self.model.running_tasks.push(name.to_string());
                }
            }
            TaskStatus::Completed
            | TaskStatus::Failed
            | TaskStatus::Cached
            | TaskStatus::Skipped => {
                if let Some(start) = task.start_time {
                    #[allow(clippy::cast_possible_truncation)]
                    let duration = start.elapsed().as_millis() as u64;
                    task.duration_ms = Some(duration);
                }
                self.model.running_tasks.retain(|t| t != name);
            }
            TaskStatus::Pending => {}
        }
    }

    const fn is_terminal_status(status: TaskStatus) -> bool {
        matches!(
            status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cached | TaskStatus::Skipped
        )
    }

    /// Add output for a task
    pub fn add_task_output(&mut self, name: &str, stream: &str, content: String) {
        if let Some(output) = self.model.outputs.get_mut(name) {
            match stream {
                "stdout" => output.add_stdout(content),
                "stderr" => output.add_stderr(content),
                unknown => {
                    tracing::debug!(
                        "Unknown stream type '{}' for task '{}', treating as stdout",
                        unknown,
                        name
                    );
                    output.add_stdout(content);
                }
            }
        } else {
            let preview: String = content.chars().take(50).collect();
            tracing::warn!(
                "Received output for unknown task '{}': {}...",
                name,
                preview
            );
        }
    }

    /// Get elapsed time since start. See [`ActivityModel::elapsed_ms`].
    #[must_use]
    pub fn elapsed_ms(&self) -> u64 {
        self.model.elapsed_ms()
    }

    /// Mark execution as complete
    pub fn complete(&mut self, success: bool, error_message: Option<String>) {
        self.model.is_complete = true;
        self.model.success = success;
        self.model.error_message = error_message;
    }

    /// Apply a [`CuenvEvent`] to the activity model half of this state.
    ///
    /// This is the single canonical entry point for event-driven mutations.
    /// The TUI funnels every task event through here so the model stays
    /// consistent regardless of how the event arrived.
    ///
    /// The method touches only activity-model fields — `selected_task`,
    /// `cursor_position`, `focused_task`, etc. are left alone so the user's
    /// in-flight UI interaction survives a flurry of events.
    ///
    /// Late or duplicate events targeting a task that has already reached
    /// a terminal status are dropped by [`Self::update_task_status`] so
    /// determinism survives clock skew and broadcast lag.
    pub fn apply_event(&mut self, event: &CuenvEvent) {
        match &event.category {
            EventCategory::Task(task_event) => self.apply_task_event(task_event),
            EventCategory::Service(_)
            | EventCategory::Ci(_)
            | EventCategory::Command(_)
            | EventCategory::Interactive(_)
            | EventCategory::System(_)
            | EventCategory::Output(_) => {}
        }
    }

    /// Apply a [`TaskEvent`] (the bulk of the lifecycle) to the model.
    fn apply_task_event(&mut self, event: &TaskEvent) {
        match event {
            TaskEvent::Started { name, .. } => {
                self.update_task_status(name, TaskStatus::Running);
            }
            TaskEvent::CacheHit { name, .. } => {
                self.update_task_status(name, TaskStatus::Cached);
            }
            TaskEvent::Output {
                name,
                stream,
                content,
                ..
            } => {
                let stream_str = match stream {
                    Stream::Stdout => "stdout",
                    Stream::Stderr => "stderr",
                };
                self.add_task_output(name, stream_str, content.clone());
            }
            TaskEvent::Completed {
                name,
                success,
                exit_code,
                ..
            } => {
                let status = if *success {
                    TaskStatus::Completed
                } else {
                    TaskStatus::Failed
                };
                self.update_task_status(name, status);
                if let Some(task) = self.model.tasks.get_mut(name) {
                    task.exit_code = *exit_code;
                }
            }
            TaskEvent::Skipped { name, .. } => {
                self.update_task_status(name, TaskStatus::Skipped);
            }
            TaskEvent::CacheMiss { .. }
            | TaskEvent::CacheSkipped { .. }
            | TaskEvent::Queued { .. }
            | TaskEvent::Retrying { .. }
            | TaskEvent::GroupStarted { .. }
            | TaskEvent::GroupCompleted { .. } => {}
        }
    }
}

impl Default for TuiState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_status_symbol() {
        assert_eq!(TaskStatus::Pending.symbol(), "⏸");
        assert_eq!(TaskStatus::Running.symbol(), "▶");
        assert_eq!(TaskStatus::Completed.symbol(), "✓");
        assert_eq!(TaskStatus::Failed.symbol(), "✗");
        assert_eq!(TaskStatus::Skipped.symbol(), "⊘");
        assert_eq!(TaskStatus::Cached.symbol(), "⚡");
    }

    #[test]
    fn test_task_info_new() {
        let task = TaskInfo::new("test".to_string(), vec!["dep1".to_string()], 1);
        assert_eq!(task.name, "test");
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.level, 1);
        assert!(task.start_time.is_none());
        assert!(task.duration_ms.is_none());
    }

    #[test]
    fn test_task_output_new() {
        let output = TaskOutput::new("test".to_string());
        assert_eq!(output.name, "test");
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        assert!(!output.stdout_dirty);
        assert!(!output.stderr_dirty);
    }

    #[test]
    fn test_task_output_add() {
        let mut output = TaskOutput::new("test".to_string());
        output.add_stdout("line1".to_string());
        output.add_stderr("error1".to_string());

        assert_eq!(output.stdout.len(), 1);
        assert_eq!(output.stderr.len(), 1);
        assert!(output.stdout_dirty);
        assert!(output.stderr_dirty);

        output.clear_dirty();
        assert!(!output.stdout_dirty);
        assert!(!output.stderr_dirty);
    }

    #[test]
    fn test_tui_state_new() {
        let state = TuiState::new();
        assert!(state.tasks().is_empty());
        assert!(state.outputs().is_empty());
        assert!(state.running_tasks().is_empty());
        assert!(!state.is_complete());
        assert!(!state.success());
    }

    #[test]
    fn test_tui_state_add_task() {
        let mut state = TuiState::new();
        let task = TaskInfo::new("test".to_string(), vec![], 0);
        state.add_task(task);

        assert_eq!(state.tasks().len(), 1);
        assert_eq!(state.outputs().len(), 1);
        assert!(state.tasks().contains_key("test"));
        assert!(state.outputs().contains_key("test"));
    }

    #[test]
    fn test_tui_state_update_status() {
        let mut state = TuiState::new();
        let task = TaskInfo::new("test".to_string(), vec![], 0);
        state.add_task(task);

        state.update_task_status("test", TaskStatus::Running);
        assert_eq!(
            state.tasks().get("test").unwrap().status,
            TaskStatus::Running
        );
        assert_eq!(state.running_tasks().len(), 1);

        state.update_task_status("test", TaskStatus::Completed);
        assert_eq!(
            state.tasks().get("test").unwrap().status,
            TaskStatus::Completed
        );
        assert_eq!(state.running_tasks().len(), 0);
    }

    #[test]
    fn terminal_status_is_monotonic() {
        let mut state = TuiState::new();
        state.add_task(TaskInfo::new("t".to_string(), vec![], 0));

        state.update_task_status("t", TaskStatus::Running);
        state.update_task_status("t", TaskStatus::Completed);
        state.update_task_status("t", TaskStatus::Running);
        assert_eq!(state.tasks()["t"].status, TaskStatus::Completed);
        assert!(state.running_tasks().is_empty());

        let mut state2 = TuiState::new();
        state2.add_task(TaskInfo::new("u".to_string(), vec![], 0));
        state2.update_task_status("u", TaskStatus::Running);
        state2.update_task_status("u", TaskStatus::Failed);
        state2.update_task_status("u", TaskStatus::Skipped);
        assert_eq!(state2.tasks()["u"].status, TaskStatus::Failed);
    }

    #[test]
    fn test_tui_state_parse_task_path() {
        // Full format: task:project:path.parts
        let path = TuiState::parse_task_path("task:cuenv:test.bdd");
        assert_eq!(path, vec!["test", "bdd"]);

        let path = TuiState::parse_task_path("task:cuenv:build");
        assert_eq!(path, vec!["build"]);

        // Two-part fallback
        let path = TuiState::parse_task_path("cuenv:test.unit");
        assert_eq!(path, vec!["test", "unit"]);
    }

    #[test]
    fn test_tui_state_name_hierarchy() {
        let mut state = TuiState::new();
        state.add_task(TaskInfo::new("task:proj:test.bdd".to_string(), vec![], 0));
        state.add_task(TaskInfo::new("task:proj:test.unit".to_string(), vec![], 0));
        state.add_task(TaskInfo::new("task:proj:build".to_string(), vec![], 0));

        let tree = state.build_name_hierarchy();

        // Root should have "test" group and "build" as a task (single-part names are tasks)
        let (root_groups, root_tasks) = tree.get("").unwrap();
        assert!(root_groups.contains(&"test".to_string()));
        // "build" is a single-part task, so it appears in tasks, not groups
        assert!(root_tasks.contains(&"task:proj:build".to_string()));

        // "test" group should have "test.bdd" and "test.unit" as children
        let (_, test_tasks) = tree.get("test").unwrap();
        assert_eq!(test_tasks.len(), 2);
    }

    #[test]
    fn test_tui_state_tree_navigation() {
        let mut state = TuiState::new();
        state.add_task(TaskInfo::new("task:proj:test.bdd".to_string(), vec![], 0));
        state.add_task(TaskInfo::new("task:proj:test.unit".to_string(), vec![], 0));
        state.init_tree();

        assert_eq!(state.cursor_position(), 0);

        let node = state.highlighted_node().unwrap();
        assert!(matches!(node.node_type, TreeNodeType::All));

        state.cursor_down();
        assert_eq!(state.cursor_position(), 1);

        state.cursor_up();
        assert_eq!(state.cursor_position(), 0);

        state.cursor_up();
        assert_eq!(state.cursor_position(), 0);
    }

    #[test]
    fn test_tui_state_output_mode() {
        let mut state = TuiState::new();
        state.add_task(TaskInfo::new("task:proj:build".to_string(), vec![], 0));
        state.init_tree();

        assert_eq!(state.output_mode(), OutputMode::All);

        state.cursor_down();
        state.select_current_node();

        assert_eq!(state.output_mode(), OutputMode::Selected);

        state.show_all_output();
        assert_eq!(state.output_mode(), OutputMode::All);
        assert!(state.selected_task().is_none());
    }

    #[test]
    fn test_tui_state_complete() {
        let mut state = TuiState::new();
        state.complete(true, None);

        assert!(state.is_complete());
        assert!(state.success());
        assert!(state.error_message().is_none());

        let mut state2 = TuiState::new();
        state2.complete(false, Some("error".to_string()));

        assert!(state2.is_complete());
        assert!(!state2.success());
        assert_eq!(state2.error_message(), Some("error"));
    }

    #[test]
    fn scroll_output_by_saturates() {
        let mut state = TuiState::new();
        assert_eq!(state.output_scroll(), 0);
        state.scroll_output_by(-10);
        assert_eq!(state.output_scroll(), 0, "scrolling up at 0 saturates");
        state.scroll_output_by(5);
        assert_eq!(state.output_scroll(), 5);
        state.scroll_output_by(-3);
        assert_eq!(state.output_scroll(), 2);
    }

    #[test]
    fn test_task_output_bounded_buffer() {
        let mut output = TaskOutput::new("test".to_string());

        // Add more lines than MAX_OUTPUT_LINES
        for i in 0..MAX_OUTPUT_LINES + 100 {
            output.add_stdout(format!("stdout line {i}"));
            output.add_stderr(format!("stderr line {i}"));
        }

        // Buffers should be capped at MAX_OUTPUT_LINES
        assert_eq!(output.stdout.len(), MAX_OUTPUT_LINES);
        assert_eq!(output.stderr.len(), MAX_OUTPUT_LINES);
        assert_eq!(output.combined.len(), MAX_OUTPUT_LINES);

        // Oldest lines should be dropped, newest should remain
        assert_eq!(
            output.stdout.back().unwrap(),
            &format!("stdout line {}", MAX_OUTPUT_LINES + 99)
        );
        assert_eq!(
            output.stderr.back().unwrap(),
            &format!("stderr line {}", MAX_OUTPUT_LINES + 99)
        );
    }

    #[test]
    fn test_task_output_chronological_order() {
        let mut output = TaskOutput::new("test".to_string());

        // Add lines in a specific order
        output.add_stdout("first stdout".to_string());
        output.add_stderr("first stderr".to_string());
        output.add_stdout("second stdout".to_string());
        output.add_stderr("second stderr".to_string());

        // Combined output should preserve insertion order
        assert_eq!(output.combined.len(), 4);
        assert_eq!(output.combined[0].content, "first stdout");
        assert!(!output.combined[0].is_stderr);
        assert_eq!(output.combined[1].content, "first stderr");
        assert!(output.combined[1].is_stderr);
        assert_eq!(output.combined[2].content, "second stdout");
        assert!(!output.combined[2].is_stderr);
        assert_eq!(output.combined[3].content, "second stderr");
        assert!(output.combined[3].is_stderr);
    }
}
