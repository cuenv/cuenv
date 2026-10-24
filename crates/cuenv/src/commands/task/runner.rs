//! Running a task graph with the executor, with or without the rich TUI, and
//! summarising the results.

use std::fmt::Write;

use cuenv_core::Result;
use cuenv_core::tasks::{TaskNode, Tasks};
use cuenv_task_exec::executor::{TASK_FAILURE_SNIPPET_LINES, summarize_task_failure};
use cuenv_task_exec::{ExecutorConfig, TaskExecutor, TaskGraph};

use crate::tui::rich::RichTui;
use crate::tui::state::TaskInfo;

#[cfg(feature = "dagger-backend")]
pub(super) fn build_task_executor(config: ExecutorConfig) -> TaskExecutor {
    TaskExecutor::with_dagger_factory(config, Some(cuenv_dagger::create_dagger_backend))
}

#[cfg(not(feature = "dagger-backend"))]
pub(super) fn build_task_executor(config: ExecutorConfig) -> TaskExecutor {
    TaskExecutor::new(config)
}

/// Execute task with rich TUI interface
///
/// Note: The executor MUST have `capture_output: true` to ensure task output
/// goes through the event system rather than directly to stdout/stderr.
pub(super) async fn execute_with_rich_tui(
    executor: &TaskExecutor,
    task_name: &str,
    task_graph: &TaskGraph,
) -> Result<String> {
    // Subscribe to the global event bus.
    // The global bus is set up during CLI initialization and receives all events
    // emitted via the emit_task_*! macros through the global tracing subscriber.
    let event_rx = crate::tracing::subscribe_global_events().ok_or_else(|| {
        cuenv_core::Error::configuration(
            "Global event bus not initialized - TUI requires event-based tracing".to_string(),
        )
    })?;

    // Create oneshot channel for TUI readiness signaling.
    // This prevents a race condition where task execution starts
    // before the TUI event loop is ready to receive events.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    // Create and initialize TUI
    let mut tui = RichTui::new(event_rx, ready_tx)
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to initialize TUI: {e}")))?;

    // Build TaskInfo structs from the task graph
    let mut task_infos = Vec::new();
    let sorted_tasks = task_graph
        .topological_sort()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to sort task graph: {e}")))?;

    // Calculate levels based on dependencies
    let mut levels: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for node in &sorted_tasks {
        let max_dep_level = node
            .task
            .depends_on
            .iter()
            .filter_map(|dep| levels.get(dep.task_name()).copied())
            .max()
            .unwrap_or(0);
        let increment = usize::from(!node.task.depends_on.is_empty());
        levels.insert(node.name.clone(), max_dep_level.saturating_add(increment));
    }

    for node in sorted_tasks {
        let task_name = node.name.clone();
        let dependencies: Vec<String> = node
            .task
            .depends_on
            .iter()
            .map(|d| d.task_name().to_string())
            .collect();
        let level = levels.get(&task_name).copied().unwrap_or(0);

        task_infos.push(TaskInfo::new(task_name, dependencies, level));
    }

    tui.init_tasks(task_infos);

    // Run TUI and task execution concurrently
    // Note: TUI run() is blocking (uses crossterm::event::poll), so we spawn_blocking
    let tui_handle = tokio::task::spawn_blocking(move || tui.run());

    // Wait for TUI to signal it's ready before starting task execution.
    // This prevents a race condition where early events are missed.
    if ready_rx.await.is_err() {
        // TUI failed to start or was dropped before signaling ready
        return Err(cuenv_core::Error::configuration(
            "TUI failed to initialize - event loop did not start".to_string(),
        ));
    }

    // Execute tasks
    let results = executor.execute_graph(task_graph).await?;

    // Determine overall success
    let all_succeeded = results.iter().all(|r| r.success);

    // Emit completion event so the TUI knows execution is done.
    // This must happen BEFORE the event bus sender is dropped.
    cuenv_events::emit_command_completed!("task", all_succeeded, 0_u64);

    // Wait for TUI to finish and handle any errors.
    // Note: No sleep is needed here because:
    // 1. The TUI polls for events every 50ms
    // 2. We're waiting for the user to dismiss the TUI (via tui_handle.await)
    // 3. The channel stays open until this function returns (after TUI finishes)
    // Note: By this point, the TUI's TerminalGuard has been dropped,
    // so the terminal is restored and stderr output will be visible.
    match tui_handle.await {
        Ok(Ok(())) => {
            // TUI completed successfully
        }
        Ok(Err(e)) => {
            // TUI returned an error - log it but don't fail the task execution
            // since the tasks themselves may have succeeded
            tracing::warn!(error = %e, "TUI error (task execution may have succeeded)");
            cuenv_events::emit_stderr!(format!("Warning: TUI encountered an error: {e}"));
            cuenv_events::emit_stderr!(
                "Task output may not have been fully displayed. Check logs for details."
            );
        }
        Err(e) => {
            // TUI task panicked or was cancelled
            tracing::error!(error = %e, "TUI task failed");
            cuenv_events::emit_stderr!(format!("Warning: TUI terminated unexpectedly: {e}"));
        }
    }

    // Check for failures
    if let Some(failed) = results.iter().find(|r| !r.success) {
        return Err(cuenv_core::Error::configuration(summarize_task_failure(
            failed,
            TASK_FAILURE_SNIPPET_LINES,
        )));
    }

    // Return success message
    Ok(format!(
        "Task '{task_name}' completed successfully in TUI mode"
    ))
}

/// Execute a task using the appropriate strategy based on task type and dependencies.
pub(super) async fn execute_task_with_strategy(
    executor: &TaskExecutor,
    task_name: &str,
    task_node: &TaskNode,
    task_graph: &TaskGraph,
    all_tasks: &Tasks,
) -> Result<Vec<cuenv_task_exec::TaskResult>> {
    match task_node {
        TaskNode::Group(_) | TaskNode::Sequence(_) => {
            // For groups (parallel) and lists (sequential), use the original execution
            executor.execute_node(task_name, task_node, all_tasks).await
        }
        TaskNode::Task(_) => {
            // The task graph is built from `all_tasks` and is the authoritative
            // dependency view for execution.
            if task_graph.task_count() <= 1 {
                executor.execute_node(task_name, task_node, all_tasks).await
            } else {
                executor.execute_graph(task_graph).await
            }
        }
    }
}

pub(super) fn format_task_results(
    results: Vec<cuenv_task_exec::TaskResult>,
    capture_output: cuenv_core::OutputCapture,
    task_name: &str,
) -> String {
    let mut output = String::new();
    for result in results {
        if capture_output.should_capture() {
            write!(output, "Task '{}' ", result.name).ok();
            if result.success {
                output.push_str("succeeded\n");
                if !result.stdout.is_empty() {
                    output.push_str("Output:\n");
                    output.push_str(&result.stdout);
                    output.push('\n');
                }
            } else {
                writeln!(output, "failed with exit code {:?}", result.exit_code).ok();
                if !result.stderr.is_empty() {
                    output.push_str("Error:\n");
                    output.push_str(&result.stderr);
                    output.push('\n');
                }
            }
        } else {
            // When not capturing output, logs are streamed directly by the executor
            // or printed from cache by the executor (if modified).
            // We do NOT print them again here to avoid duplication.
        }
    }

    if capture_output.should_capture() && output.is_empty() {
        output = format!("Task '{task_name}' completed");
    } else if !capture_output.should_capture() {
        // In non-capturing mode, ensure we always include a clear completion
        // message even if we printed cached logs above.
        if output.is_empty() {
            output = format!("Task '{task_name}' completed");
        } else {
            let _ = writeln!(output, "Task '{task_name}' completed");
        }
    }

    output
}
