//! Centralized state management for the rich TUI

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

/// Status of a task in the execution graph
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    /// Task is waiting for dependencies
    Pending,
    /// Task is currently running
    Running,
    /// Task completed successfully
    Completed,
    /// Task failed
    Failed,
    /// Task was skipped due to dependency failure
    Skipped,
    /// Task result was retrieved from cache
    Cached,
}

impl TaskStatus {
    /// Get the display symbol for this status
    #[must_use]
    pub fn symbol(&self) -> &'static str {
        match self {
            TaskStatus::Pending => "⏸",
            TaskStatus::Running => "▶",
            TaskStatus::Completed => "✓",
            TaskStatus::Failed => "✗",
            TaskStatus::Skipped => "⊘",
            TaskStatus::Cached => "⚡",
        }
    }

    /// Get the color for this status (as ratatui Color)
    #[must_use]
    pub fn color(&self) -> ratatui::style::Color {
        use ratatui::style::Color;
        match self {
            TaskStatus::Pending => Color::DarkGray,
            TaskStatus::Running => Color::Yellow,
            TaskStatus::Completed => Color::Green,
            TaskStatus::Failed => Color::Red,
            TaskStatus::Skipped => Color::DarkGray,
            TaskStatus::Cached => Color::Cyan,
        }
    }
}

/// Information about a task in the execution graph
#[derive(Debug, Clone)]
pub struct TaskInfo {
    /// Task name
    pub name: String,
    /// Current status
    pub status: TaskStatus,
    /// Task dependencies (names of tasks this depends on)
    pub dependencies: Vec<String>,
    /// Level in the DAG (for visualization)
    pub level: usize,
    /// Start time (if started)
    pub start_time: Option<Instant>,
    /// Duration in milliseconds (if completed)
    pub duration_ms: Option<u64>,
    /// Exit code (if completed)
    pub exit_code: Option<i32>,
}

impl TaskInfo {
    /// Create a new task info
    #[must_use]
    pub fn new(name: String, dependencies: Vec<String>, level: usize) -> Self {
        Self {
            name,
            status: TaskStatus::Pending,
            dependencies,
            level,
            start_time: None,
            duration_ms: None,
            exit_code: None,
        }
    }

    /// Get elapsed time in milliseconds for running tasks
    #[must_use]
    pub fn elapsed_ms(&self) -> Option<u64> {
        self.start_time
            .map(|start| start.elapsed().as_millis() as u64)
    }
}

/// Maximum number of output lines to keep per stream (stdout/stderr)
const MAX_OUTPUT_LINES: usize = 1000;

/// Output line with timestamp for chronological ordering
#[derive(Debug, Clone)]
pub struct OutputLine {
    /// The line content
    pub content: String,
    /// Whether this is stderr (false = stdout)
    pub is_stderr: bool,
    /// Sequence number for ordering
    pub sequence: usize,
}

/// Output buffer for a running task
#[derive(Debug, Clone)]
pub struct TaskOutput {
    /// Task name
    pub name: String,
    /// Stdout lines (bounded ring buffer)
    pub stdout: VecDeque<String>,
    /// Stderr lines (bounded ring buffer)
    pub stderr: VecDeque<String>,
    /// Combined output with ordering preserved
    pub combined: VecDeque<OutputLine>,
    /// Whether stdout has new content since last render
    pub stdout_dirty: bool,
    /// Whether stderr has new content since last render
    pub stderr_dirty: bool,
    /// Sequence counter for ordering
    sequence: usize,
}

impl TaskOutput {
    /// Create a new task output buffer
    #[must_use]
    pub fn new(name: String) -> Self {
        Self {
            name,
            stdout: VecDeque::new(),
            stderr: VecDeque::new(),
            combined: VecDeque::new(),
            stdout_dirty: false,
            stderr_dirty: false,
            sequence: 0,
        }
    }

    /// Add a stdout line (with bounded buffer)
    pub fn add_stdout(&mut self, line: String) {
        // Add to stdout buffer with size limit
        if self.stdout.len() >= MAX_OUTPUT_LINES {
            self.stdout.pop_front();
        }
        self.stdout.push_back(line.clone());

        // Add to combined output with ordering
        if self.combined.len() >= MAX_OUTPUT_LINES {
            self.combined.pop_front();
        }
        self.combined.push_back(OutputLine {
            content: line,
            is_stderr: false,
            sequence: self.sequence,
        });
        self.sequence = self.sequence.wrapping_add(1);

        self.stdout_dirty = true;
    }

    /// Add a stderr line (with bounded buffer)
    pub fn add_stderr(&mut self, line: String) {
        // Add to stderr buffer with size limit
        if self.stderr.len() >= MAX_OUTPUT_LINES {
            self.stderr.pop_front();
        }
        self.stderr.push_back(line.clone());

        // Add to combined output with ordering
        if self.combined.len() >= MAX_OUTPUT_LINES {
            self.combined.pop_front();
        }
        self.combined.push_back(OutputLine {
            content: line,
            is_stderr: true,
            sequence: self.sequence,
        });
        self.sequence = self.sequence.wrapping_add(1);

        self.stderr_dirty = true;
    }

    /// Clear dirty flags after rendering
    pub fn clear_dirty(&mut self) {
        self.stdout_dirty = false;
        self.stderr_dirty = false;
    }
}

/// Global TUI state for task execution
#[derive(Debug)]
pub struct TuiState {
    /// Start time of the overall execution
    pub start_time: Instant,
    /// Map of task name to task info
    pub tasks: HashMap<String, TaskInfo>,
    /// Map of task name to output buffer
    pub outputs: HashMap<String, TaskOutput>,
    /// Currently running tasks (for split-screen display)
    pub running_tasks: Vec<String>,
    /// Whether execution is complete
    pub is_complete: bool,
    /// Overall success status
    pub success: bool,
    /// Error message (if failed)
    pub error_message: Option<String>,
    /// Maximum number of parallel tasks to show in panes
    pub max_parallel_panes: usize,
}

impl TuiState {
    /// Create a new TUI state
    #[must_use]
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            tasks: HashMap::new(),
            outputs: HashMap::new(),
            running_tasks: Vec::new(),
            is_complete: false,
            success: false,
            error_message: None,
            max_parallel_panes: 4,
        }
    }

    /// Add a task to the state
    pub fn add_task(&mut self, task: TaskInfo) {
        let name = task.name.clone();
        self.tasks.insert(name.clone(), task);
        self.outputs.insert(name.clone(), TaskOutput::new(name));
    }

    /// Update task status
    pub fn update_task_status(&mut self, name: &str, status: TaskStatus) {
        if let Some(task) = self.tasks.get_mut(name) {
            task.status = status;

            match status {
                TaskStatus::Running => {
                    task.start_time = Some(Instant::now());
                    if !self.running_tasks.contains(&name.to_string()) {
                        self.running_tasks.push(name.to_string());
                    }
                }
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cached | TaskStatus::Skipped => {
                    if let Some(start) = task.start_time {
                        task.duration_ms = Some(start.elapsed().as_millis() as u64);
                    }
                    self.running_tasks.retain(|t| t != name);
                }
                _ => {}
            }
        }
    }

    /// Add output for a task
    pub fn add_task_output(&mut self, name: &str, stream: &str, content: String) {
        if let Some(output) = self.outputs.get_mut(name) {
            match stream {
                "stdout" => output.add_stdout(content),
                "stderr" => output.add_stderr(content),
                _ => {}
            }
        }
    }

    /// Get tasks grouped by level (for DAG visualization)
    #[must_use]
    pub fn tasks_by_level(&self) -> Vec<Vec<&TaskInfo>> {
        let max_level = self.tasks.values().map(|t| t.level).max().unwrap_or(0);
        let mut levels = vec![Vec::new(); max_level + 1];

        for task in self.tasks.values() {
            levels[task.level].push(task);
        }

        levels
    }

    /// Get elapsed time since start
    #[must_use]
    pub fn elapsed_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }

    /// Get visible running tasks (limited by max_parallel_panes)
    #[must_use]
    pub fn visible_running_tasks(&self) -> Vec<&str> {
        self.running_tasks
            .iter()
            .take(self.max_parallel_panes)
            .map(|s| s.as_str())
            .collect()
    }

    /// Mark execution as complete
    pub fn complete(&mut self, success: bool, error_message: Option<String>) {
        self.is_complete = true;
        self.success = success;
        self.error_message = error_message;
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
        assert!(state.tasks.is_empty());
        assert!(state.outputs.is_empty());
        assert!(state.running_tasks.is_empty());
        assert!(!state.is_complete);
        assert!(!state.success);
    }

    #[test]
    fn test_tui_state_add_task() {
        let mut state = TuiState::new();
        let task = TaskInfo::new("test".to_string(), vec![], 0);
        state.add_task(task);

        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.outputs.len(), 1);
        assert!(state.tasks.contains_key("test"));
        assert!(state.outputs.contains_key("test"));
    }

    #[test]
    fn test_tui_state_update_status() {
        let mut state = TuiState::new();
        let task = TaskInfo::new("test".to_string(), vec![], 0);
        state.add_task(task);

        state.update_task_status("test", TaskStatus::Running);
        assert_eq!(state.tasks.get("test").unwrap().status, TaskStatus::Running);
        assert_eq!(state.running_tasks.len(), 1);

        state.update_task_status("test", TaskStatus::Completed);
        assert_eq!(state.tasks.get("test").unwrap().status, TaskStatus::Completed);
        assert_eq!(state.running_tasks.len(), 0);
    }

    #[test]
    fn test_tui_state_tasks_by_level() {
        let mut state = TuiState::new();
        state.add_task(TaskInfo::new("task1".to_string(), vec![], 0));
        state.add_task(TaskInfo::new("task2".to_string(), vec!["task1".to_string()], 1));
        state.add_task(TaskInfo::new("task3".to_string(), vec!["task1".to_string()], 1));

        let levels = state.tasks_by_level();
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].len(), 1);
        assert_eq!(levels[1].len(), 2);
    }

    #[test]
    fn test_tui_state_complete() {
        let mut state = TuiState::new();
        state.complete(true, None);

        assert!(state.is_complete);
        assert!(state.success);
        assert!(state.error_message.is_none());

        let mut state2 = TuiState::new();
        state2.complete(false, Some("error".to_string()));

        assert!(state2.is_complete);
        assert!(!state2.success);
        assert_eq!(state2.error_message, Some("error".to_string()));
    }

    #[test]
    fn test_task_output_bounded_buffer() {
        let mut output = TaskOutput::new("test".to_string());

        // Add more lines than MAX_OUTPUT_LINES
        for i in 0..MAX_OUTPUT_LINES + 100 {
            output.add_stdout(format!("stdout line {}", i));
            output.add_stderr(format!("stderr line {}", i));
        }

        // Buffers should be capped at MAX_OUTPUT_LINES
        assert_eq!(output.stdout.len(), MAX_OUTPUT_LINES);
        assert_eq!(output.stderr.len(), MAX_OUTPUT_LINES);
        assert_eq!(output.combined.len(), MAX_OUTPUT_LINES);

        // Oldest lines should be dropped, newest should remain
        assert_eq!(output.stdout.back().unwrap(), &format!("stdout line {}", MAX_OUTPUT_LINES + 99));
        assert_eq!(output.stderr.back().unwrap(), &format!("stderr line {}", MAX_OUTPUT_LINES + 99));
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
