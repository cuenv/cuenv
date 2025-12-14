# Rich TUI for Task Execution

This module implements a rich terminal user interface (TUI) for visualizing task execution with:

- **DAG Visualization**: Shows task dependencies in a hierarchical layout
- **Split-Screen Output**: Displays multiple task outputs simultaneously
- **Real-Time Updates**: Live status updates as tasks execute
- **Color-Coded Display**: Different colors for pending, running, completed, failed, cached, and skipped tasks

## Architecture

```
┌─────────────────────────────────────────────┐
│ Header (Elapsed Time)                       │
├─────────────────────────────────────────────┤
│ DAG Visualization (25% height)              │
│   Level 0: task1 ▶  task2 ✓                │
│   Level 1: task3 ⏸                         │
├─────────────────────────────────────────────┤
│ Task Output Panes (65% height)              │
│ ┌───────────┬───────────┬───────────┐      │
│ │ task1 ▶  │ task2 ✓   │ task3 ⏸  │      │
│ │ output... │ output... │ output... │      │
│ └───────────┴───────────┴───────────┘      │
├─────────────────────────────────────────────┤
│ Status Bar (Task Counts, Keyboard Help)     │
└─────────────────────────────────────────────┘
```

## Components

### Widgets (`widgets/`)

- **`dag.rs`**: DAG visualization widget
  - `TaskStatus`: Status enum (Pending, Running, Completed, Failed, Skipped, Cached)
  - `DagState`: Manages task nodes and dependency levels
  - `DagWidget`: Renders the DAG

- **`task_panes.rs`**: Split-screen task output
  - `TaskOutputBuffer`: Circular buffer for task output (configurable max lines)
  - `TaskPanesState`: Manages multiple task outputs
  - `TaskPanesWidget`: Renders task panes side-by-side

### State (`state.rs`)

- **`RichTuiState`**: Centralized state combining:
  - DAG visualization state
  - Task output buffers
  - Timing information
  - Completion status

### Rich TUI (`rich.rs`)

- **`run_rich_tui()`**: Main entry point for the rich TUI
  - Handles keyboard input (q/Esc to quit, Ctrl+C to abort)
  - Processes cuenv events from coordinator
  - Renders full-screen TUI

## Usage

### Basic Integration

```rust
use cuenv::tui::{run_rich_tui, RichTuiState};
use cuenv_core::tasks::TaskGraph;
use coordinator::client::CoordinatorClient;

// 1. Build your task graph
let mut graph = TaskGraph::new();
graph.build_for_task("my-task", &all_tasks)?;

// 2. Initialize TUI state from graph
let mut state = RichTuiState::new(1000, 8);
state.init_from_graph(&graph)?;

// 3. Connect to coordinator and run rich TUI
let mut client = CoordinatorClient::connect().await?;
run_rich_tui(&mut client).await?;
```

### Event Handling

The rich TUI automatically handles these cuenv events:

- `TaskEvent::Started` → Updates task status to Running, marks as active
- `TaskEvent::CacheHit` → Updates task status to Cached
- `TaskEvent::Output` → Appends output line to task buffer
- `TaskEvent::Completed` → Updates task status to Completed/Failed
- `SystemEvent::Shutdown` → Marks execution as complete

### Task Status Indicators

| Symbol | Status    | Color     | Description                |
|--------|-----------|-----------|----------------------------|
| ⏸      | Pending   | Gray      | Task not started yet       |
| ▶      | Running   | Blue      | Task currently executing   |
| ✓      | Completed | Green     | Task completed successfully|
| ✗      | Failed    | Red       | Task failed                |
| ⊘      | Skipped   | Yellow    | Task skipped               |
| ⚡      | Cached    | Cyan      | Task result from cache     |

### Keyboard Controls

- `q` or `Esc`: Quit (after execution completes)
- `Ctrl+C`: Force abort

## Configuration

The `RichTuiState` can be configured with:

```rust
// max_lines_per_buffer: Maximum output lines per task (default: 1000)
// max_panes: Maximum task panes to show simultaneously (default: 8)
let state = RichTuiState::new(max_lines_per_buffer, max_panes);
```

## Future Enhancements

Planned features (see issue #131):

- [ ] Interactive navigation (arrow keys, tab to cycle tasks)
- [ ] Display mode toggles (DAG, focus mode, all tasks)
- [ ] Output filtering (running, errors, completed)
- [ ] Scrolling support for long outputs
- [ ] Configuration via CUE (ui mode, refresh rate, etc.)
- [ ] Export DAG as SVG/PNG
- [ ] Auto-detect terminal capabilities

## Testing

Run widget tests:
```bash
cargo test --package cuenv --lib tui::widgets
```

Run state tests:
```bash
cargo test --package cuenv --lib tui::state
```

Run rich TUI tests:
```bash
cargo test --package cuenv --lib tui::rich
```
