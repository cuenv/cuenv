//! Task execution command implementation

mod arguments;
mod dag_export;
mod discovery;
mod execution;
pub mod list_builder;
mod rendering;
mod types;

// Re-export types for the public API. Some types may not be used externally yet.
pub use types::{ExecutionMode, OutputConfig, TaskExecutionRequest, TaskSelection};

use cuenv_core::Result;
use cuenv_core::lockfile::{LOCKFILE_NAME, LockedRuntime, Lockfile};
use cuenv_core::manifest::Runtime;
use cuenv_core::tasks::cache::TaskCacheConfig;
use cuenv_core::tasks::executor::{TASK_FAILURE_SNIPPET_LINES, summarize_task_failure};
use cuenv_core::tasks::{ExecutorConfig, TaskExecutor, TaskGraph, TaskNode, Tasks};
use std::collections::BTreeMap;
use std::sync::Arc;

use super::relative_path_from_root;
use crate::tui::rich::RichTui;
use crate::tui::state::TaskInfo;

#[cfg(feature = "dagger-backend")]
fn build_task_executor(config: ExecutorConfig) -> TaskExecutor {
    TaskExecutor::with_dagger_factory(config, Some(cuenv_dagger::create_dagger_backend))
}

#[cfg(not(feature = "dagger-backend"))]
fn build_task_executor(config: ExecutorConfig) -> TaskExecutor {
    TaskExecutor::new(config)
}
use std::fmt::Write;
use std::path::{Path, PathBuf};

use tracing::instrument;

/// Resolve the on-disk root for the local CAS + action cache.
///
/// Resolution order:
/// 1. `$CUENV_CACHE_DIR` (explicit override)
/// 2. `$XDG_CACHE_HOME/cuenv` or the platform default
/// 3. `<project>/.cuenv-cache`
fn resolve_cache_root(project_root: &Path) -> PathBuf {
    if let Some(env) = std::env::var_os("CUENV_CACHE_DIR")
        && !env.is_empty()
    {
        return PathBuf::from(env);
    }
    if let Some(d) = dirs::cache_dir() {
        return d.join("cuenv");
    }
    project_root.join(".cuenv-cache")
}

/// Construct the [`TaskCacheConfig`] used by the executor.
///
/// Returns `None` if the local CAS or action cache cannot be opened (e.g.
/// permissions). In that case the executor falls back to the no-cache code
/// path so the user's command still works — degraded, not broken.
fn build_task_cache(
    project_root: &Path,
    runtime_identity: RuntimeCacheIdentity,
) -> Option<TaskCacheConfig> {
    let root = resolve_cache_root(project_root);
    let cas = match cuenv_cas::LocalCas::open(&root) {
        Ok(c) => Arc::new(c) as Arc<dyn cuenv_cas::Cas>,
        Err(e) => {
            tracing::warn!(error = %e, root = %root.display(), "task cache disabled: cannot open CAS");
            return None;
        }
    };
    let action_cache = match cuenv_cas::LocalActionCache::open(&root) {
        Ok(ac) => Arc::new(ac) as Arc<dyn cuenv_cas::ActionCache>,
        Err(e) => {
            tracing::warn!(error = %e, root = %root.display(), "task cache disabled: cannot open action cache");
            return None;
        }
    };
    let vcs_hasher =
        Arc::new(cuenv_vcs::WalkHasher::new(project_root)) as Arc<dyn cuenv_vcs::VcsHasher>;
    Some(TaskCacheConfig {
        cas,
        action_cache,
        vcs_hasher,
        vcs_hasher_root: project_root.to_path_buf(),
        cuenv_version: env!("CARGO_PKG_VERSION").to_string(),
        runtime_identity_properties: runtime_identity.properties,
        cache_disabled_reason: runtime_identity.cache_disabled_reason,
    })
}

#[derive(Debug, Clone, Default)]
struct RuntimeCacheIdentity {
    properties: BTreeMap<String, String>,
    cache_disabled_reason: Option<String>,
}

fn resolve_runtime_cache_identity(
    module_root: &Path,
    project_root: &Path,
    runtime: Option<&Runtime>,
) -> RuntimeCacheIdentity {
    let mut identity = RuntimeCacheIdentity::default();
    let Some(runtime) = runtime else {
        return identity;
    };

    match runtime {
        Runtime::Nix(nix_runtime) => {
            identity
                .properties
                .insert("runtime.kind".to_string(), "nix".to_string());

            let lockfile_path = module_root.join(LOCKFILE_NAME);
            let lockfile = match Lockfile::load(&lockfile_path) {
                Ok(Some(lockfile)) => lockfile,
                Ok(None) => {
                    identity.cache_disabled_reason = Some(format!(
                        "runtime is nix but {} is missing",
                        lockfile_path.display()
                    ));
                    return identity;
                }
                Err(e) => {
                    identity.cache_disabled_reason = Some(format!(
                        "runtime is nix but {} could not be read: {}",
                        lockfile_path.display(),
                        e
                    ));
                    return identity;
                }
            };

            let project_path = relative_path_from_root(module_root, project_root);
            let project_key = project_path.to_string_lossy().into_owned();
            let Some(locked_runtime) = lockfile.find_runtime(&project_key) else {
                identity.cache_disabled_reason = Some(format!(
                    "runtime is nix but lockfile has no runtime entry for project '{}'",
                    project_key
                ));
                return identity;
            };

            let LockedRuntime::Nix(locked_nix) = locked_runtime;

            if locked_nix.flake != nix_runtime.flake || locked_nix.output != nix_runtime.output {
                identity.cache_disabled_reason = Some(format!(
                    "runtime lock mismatch for project '{}': expected flake='{}' output='{}', got flake='{}' output='{}'",
                    project_key,
                    nix_runtime.flake,
                    nix_runtime.output.as_deref().unwrap_or(""),
                    locked_nix.flake,
                    locked_nix.output.as_deref().unwrap_or("")
                ));
                return identity;
            }

            identity
                .properties
                .insert("runtime.nix.digest".to_string(), locked_nix.digest.clone());
            identity
                .properties
                .insert("runtime.nix.flake".to_string(), locked_nix.flake.clone());
            if let Some(output) = &locked_nix.output {
                identity
                    .properties
                    .insert("runtime.nix.output".to_string(), output.clone());
            }
            identity.properties.insert(
                "runtime.nix.lockfile".to_string(),
                locked_nix.lockfile.clone(),
            );
            identity
        }
        Runtime::Devenv(_) => {
            identity
                .properties
                .insert("runtime.kind".to_string(), "devenv".to_string());
            identity
        }
        Runtime::Container(_) => {
            identity
                .properties
                .insert("runtime.kind".to_string(), "container".to_string());
            identity
        }
        Runtime::Dagger(_) => {
            identity
                .properties
                .insert("runtime.kind".to_string(), "dagger".to_string());
            identity
        }
        Runtime::Oci(_) => {
            identity
                .properties
                .insert("runtime.kind".to_string(), "oci".to_string());
            identity
        }
        Runtime::Tools(_) => {
            identity
                .properties
                .insert("runtime.kind".to_string(), "tools".to_string());
            identity
        }
    }
}

/// Execute a task using the new structured request API.
///
/// This is the preferred entry point for task execution. It accepts a
/// `TaskExecutionRequest` which groups all parameters into a structured
/// format with type-safe selection modes.
///
/// # Errors
///
/// Returns an error if task resolution, validation, or execution fails.
///
/// # Example
///
/// ```ignore
/// let request = TaskExecutionRequest::named("./", "cuenv", "build")
///     .with_args(vec!["--release".to_string()])
///     .with_environment("prod");
///
/// let output = execute(request).await?;
/// ```
#[instrument(name = "task_execute", skip(request), fields(path = %request.path, package = %request.package))]
pub async fn execute(request: TaskExecutionRequest<'_>) -> Result<String> {
    execution::execute_task_impl(&request).await
}

/// Execute task with rich TUI interface
///
/// Note: The executor MUST have `capture_output: true` to ensure task output
/// goes through the event system rather than directly to stdout/stderr.
async fn execute_with_rich_tui(
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
async fn execute_task_with_strategy(
    executor: &TaskExecutor,
    task_name: &str,
    task_node: &TaskNode,
    task_graph: &TaskGraph,
    all_tasks: &Tasks,
) -> Result<Vec<cuenv_core::tasks::TaskResult>> {
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

fn format_task_results(
    results: Vec<cuenv_core::tasks::TaskResult>,
    capture_output: cuenv_core::OutputCapture,
    task_name: &str,
) -> String {
    let mut output = String::new();
    for result in results {
        if capture_output.should_capture() {
            write!(output, "Task '{}' ", result.name).expect("write to string");
            if result.success {
                output.push_str("succeeded\n");
                if !result.stdout.is_empty() {
                    output.push_str("Output:\n");
                    output.push_str(&result.stdout);
                    output.push('\n');
                }
            } else {
                writeln!(output, "failed with exit code {:?}", result.exit_code)
                    .expect("write to string");
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

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
