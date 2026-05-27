use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use super::elapsed_millis_since;

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
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Pending => "⏸",
            Self::Running => "▶",
            Self::Completed => "✓",
            Self::Failed => "✗",
            Self::Skipped => "⊘",
            Self::Cached => "⚡",
        }
    }

    /// Get the color for this status (as ratatui Color)
    #[must_use]
    pub const fn color(self) -> ratatui::style::Color {
        use ratatui::style::Color;
        match self {
            Self::Running => Color::Yellow,
            Self::Completed => Color::Green,
            Self::Failed => Color::Red,
            Self::Pending | Self::Skipped => Color::DarkGray,
            Self::Cached => Color::Cyan,
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
    pub const fn new(name: String, dependencies: Vec<String>, level: usize) -> Self {
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
        self.start_time.map(elapsed_millis_since)
    }
}

/// Maximum number of output lines to keep per stream (stdout/stderr)
pub(super) const MAX_OUTPUT_LINES: usize = 1000;

/// Output line in the combined output buffer.
///
/// Lines are stored in insertion order (via `VecDeque::push_back`), which
/// naturally preserves chronological order since events arrive sequentially.
#[derive(Debug, Clone)]
pub struct OutputLine {
    /// The line content
    pub content: String,
    /// Whether this is stderr (false = stdout)
    pub is_stderr: bool,
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
    /// Combined output with ordering preserved (insertion order = chronological)
    pub combined: VecDeque<OutputLine>,
    /// Whether stdout has new content since last render
    pub stdout_dirty: bool,
    /// Whether stderr has new content since last render
    pub stderr_dirty: bool,
}

impl TaskOutput {
    /// Create a new task output buffer
    #[must_use]
    pub const fn new(name: String) -> Self {
        Self {
            name,
            stdout: VecDeque::new(),
            stderr: VecDeque::new(),
            combined: VecDeque::new(),
            stdout_dirty: false,
            stderr_dirty: false,
        }
    }

    /// Add a line to the appropriate buffer (with bounded buffer management)
    fn add_line(&mut self, line: String, is_stderr: bool) {
        let (buffer, dirty_flag) = if is_stderr {
            (&mut self.stderr, &mut self.stderr_dirty)
        } else {
            (&mut self.stdout, &mut self.stdout_dirty)
        };

        if buffer.len() >= MAX_OUTPUT_LINES {
            buffer.pop_front();
        }
        buffer.push_back(line.clone());

        if self.combined.len() >= MAX_OUTPUT_LINES {
            self.combined.pop_front();
        }
        self.combined.push_back(OutputLine {
            content: line,
            is_stderr,
        });

        *dirty_flag = true;
    }

    /// Add a stdout line (with bounded buffer)
    pub fn add_stdout(&mut self, line: String) {
        self.add_line(line, false);
    }

    /// Add a stderr line (with bounded buffer)
    pub fn add_stderr(&mut self, line: String) {
        self.add_line(line, true);
    }

    /// Clear dirty flags after rendering
    pub const fn clear_dirty(&mut self) {
        self.stdout_dirty = false;
        self.stderr_dirty = false;
    }
}

/// Event-driven activity state.
///
/// Mutated only by `TuiState::apply_event` and the small set of helpers
/// it delegates to. Two replays of the same event trace produce
/// identical `ActivityModel`s — the integration tests in
/// `tests/tui_replay.rs` exercise that invariant.
///
/// Fields are module-visible so `TuiState` remains the only mutation boundary.
#[derive(Debug)]
pub struct ActivityModel {
    pub(super) start_time: Instant,
    pub(super) tasks: HashMap<String, TaskInfo>,
    pub(super) outputs: HashMap<String, TaskOutput>,
    pub(super) running_tasks: Vec<String>,
    pub(super) is_complete: bool,
    pub(super) success: bool,
    pub(super) error_message: Option<String>,
}

impl ActivityModel {
    pub(super) fn new() -> Self {
        Self {
            start_time: Instant::now(),
            tasks: HashMap::new(),
            outputs: HashMap::new(),
            running_tasks: Vec::new(),
            is_complete: false,
            success: false,
            error_message: None,
        }
    }

    /// Map of task name to task info.
    #[must_use]
    pub const fn tasks(&self) -> &HashMap<String, TaskInfo> {
        &self.tasks
    }

    /// Map of task name to output buffer.
    #[must_use]
    pub const fn outputs(&self) -> &HashMap<String, TaskOutput> {
        &self.outputs
    }

    /// Names of tasks currently in `Running` state, in start order.
    #[must_use]
    pub fn running_tasks(&self) -> &[String] {
        &self.running_tasks
    }

    /// Whether the overall execution has reported completion.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.is_complete
    }

    /// Whether the completed run was successful.
    #[must_use]
    pub const fn success(&self) -> bool {
        self.success
    }

    /// Optional human-readable error message attached at completion.
    #[must_use]
    pub fn error_message(&self) -> Option<&str> {
        self.error_message.as_deref()
    }

    /// Milliseconds elapsed since [`ActivityModel`] construction.
    #[must_use]
    pub fn elapsed_ms(&self) -> u64 {
        elapsed_millis_since(self.start_time)
    }
}
