//! Centralized TUI state management

use super::widgets::dag::{DagState, TaskStatus};
use super::widgets::task_panes::TaskPanesState;
use cuenv_core::tasks::TaskGraph;
use std::time::Instant;

/// Rich TUI state combining DAG and task output state
#[derive(Debug, Clone)]
pub struct RichTuiState {
    /// DAG visualization state
    pub dag_state: DagState,
    /// Task output panes state
    pub panes_state: TaskPanesState,
    /// When the TUI was started
    pub start_time: Instant,
    /// Whether execution is complete
    pub is_complete: bool,
    /// Overall success status
    pub overall_success: Option<bool>,
}

impl RichTuiState {
    /// Create a new rich TUI state
    pub fn new(max_lines_per_buffer: usize, max_panes: usize) -> Self {
        Self {
            dag_state: DagState::new(),
            panes_state: TaskPanesState::new(max_lines_per_buffer, max_panes),
            start_time: Instant::now(),
            is_complete: false,
            overall_success: None,
        }
    }

    /// Add a task to the DAG
    pub fn add_dag_task(&mut self, name: String, level: usize, position: usize) {
        self.dag_state
            .add_node(name, TaskStatus::Pending, level, position);
    }

    /// Update task status in both DAG and panes
    pub fn update_task_status(&mut self, name: &str, status: TaskStatus) {
        self.dag_state.update_status(name, status);
        self.panes_state.update_status(name, status);

        // Mark task as active when it starts running
        if status == TaskStatus::Running {
            self.panes_state.mark_active(name);
        }
    }

    /// Add output line to a task
    pub fn push_task_output(&mut self, name: &str, line: String) {
        self.panes_state.push_output(name, line);
    }

    /// Mark execution as complete
    pub fn set_complete(&mut self, success: bool) {
        self.is_complete = true;
        self.overall_success = Some(success);
    }

    /// Get elapsed time since start
    pub fn elapsed(&self) -> std::time::Duration {
        self.start_time.elapsed()
    }

    /// Initialize DAG state from a TaskGraph
    ///
    /// This function extracts parallel groups from the task graph and populates
    /// the DAG state with all tasks at their appropriate dependency levels.
    pub fn init_from_graph(&mut self, graph: &TaskGraph) -> Result<(), String> {
        // Get parallel groups (dependency levels)
        let parallel_groups = graph
            .get_parallel_groups()
            .map_err(|e| format!("Failed to get parallel groups: {e}"))?;

        // Add all tasks to DAG state
        for (level, nodes) in parallel_groups.iter().enumerate() {
            for (position, node) in nodes.iter().enumerate() {
                self.add_dag_task(node.name.clone(), level, position);
            }
        }

        Ok(())
    }
}

impl Default for RichTuiState {
    fn default() -> Self {
        Self::new(1000, 8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rich_tui_state_new() {
        let state = RichTuiState::new(500, 4);
        assert_eq!(state.panes_state.max_lines_per_buffer, 500);
        assert_eq!(state.panes_state.max_panes, 4);
        assert!(!state.is_complete);
        assert!(state.overall_success.is_none());
    }

    #[test]
    fn test_add_dag_task() {
        let mut state = RichTuiState::default();
        state.add_dag_task("task1".to_string(), 0, 0);
        state.add_dag_task("task2".to_string(), 1, 0);

        assert_eq!(state.dag_state.nodes.len(), 2);
        assert_eq!(state.dag_state.max_level, 1);
    }

    #[test]
    fn test_update_task_status() {
        let mut state = RichTuiState::default();
        state.add_dag_task("task1".to_string(), 0, 0);

        state.update_task_status("task1", TaskStatus::Running);

        assert_eq!(
            state.dag_state.nodes[0].status,
            TaskStatus::Running
        );
        assert!(state.panes_state.active_tasks.contains(&"task1".to_string()));
    }

    #[test]
    fn test_push_task_output() {
        let mut state = RichTuiState::default();
        state.push_task_output("task1", "line1".to_string());
        state.push_task_output("task1", "line2".to_string());

        assert!(state.panes_state.buffers.contains_key("task1"));
        let buffer = &state.panes_state.buffers["task1"];
        assert_eq!(buffer.output.len(), 2);
    }

    #[test]
    fn test_set_complete() {
        let mut state = RichTuiState::default();
        state.set_complete(true);

        assert!(state.is_complete);
        assert_eq!(state.overall_success, Some(true));
    }
}
