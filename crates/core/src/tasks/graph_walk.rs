//! Generic parallel-DAG walker shared by `cuenv task` and `cuenv ci`.
//!
//! Both schedulers walk a [`TaskGraph`] by topological "parallel groups"
//! and run independent tasks at the same depth concurrently, bounded by a
//! configurable parallelism cap. They differ in *what* each task does
//! (host backend vs IR compile + run) and *how* a task's args/env are
//! resolved (output-ref placeholders vs static), but the walking, taint
//! propagation, and event emission logic is shared — and lived in two
//! places for a while, drifting on details like the queued event
//! semantics. This module is the single source of truth.
//!
//! Callers supply two closures:
//!
//! - `prepare_node` — synchronous, runs immediately before the node is
//!   spawned. Receives the [`GraphNode`] plus a read-only map of all
//!   outcomes from prior parallel groups, so a caller that needs to
//!   resolve output references can rewrite the node's task here.
//! - `run_node` — async, runs the per-task work and returns a typed
//!   outcome. The walker only inspects `is_success()`; everything else
//!   the caller may need (stdout, captures, exit code) lives in the
//!   outcome type.

use std::collections::{HashMap, HashSet};
use std::future::Future;

use cuenv_task_graph::{GraphNode, TaskGraph, TaskNodeData};
use tokio::task::JoinSet;

/// Policy controlling how the walker handles failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalkPolicy {
    /// Maximum tasks running concurrently within a single dependency level.
    /// `0` means unlimited.
    pub max_parallel: usize,
    /// When `true`, a failing task does not abort the run. Its dependents
    /// in later parallel groups are emitted as `task.skipped` and
    /// unrelated sibling chains keep running.
    pub continue_on_error: bool,
}

impl WalkPolicy {
    /// Fail-fast policy with unlimited parallelism (the historical default).
    #[must_use]
    pub const fn fail_fast() -> Self {
        Self {
            max_parallel: 0,
            continue_on_error: false,
        }
    }
}

impl Default for WalkPolicy {
    fn default() -> Self {
        Self::fail_fast()
    }
}

/// A typed outcome the walker can interpret.
pub trait WalkOutcome {
    /// Whether the per-task work reported success.
    fn is_success(&self) -> bool;
}

/// Result of a `walk_parallel_graph` call.
#[derive(Debug)]
pub struct WalkSummary<O> {
    /// Outcomes in completion order.
    pub outcomes: Vec<(String, O)>,
    /// Names of tasks whose dependencies were tainted and so were
    /// skipped rather than spawned.
    pub skipped: Vec<String>,
    /// Number of tasks whose outcome reported `!is_success()`.
    pub failed: usize,
}

/// Walk `graph` by parallel groups, preparing each node via
/// `prepare_node` and running it via `run_node`, respecting the
/// supplied [`WalkPolicy`].
///
/// `prepare_node` receives a freshly-cloned [`GraphNode`] and a
/// snapshot of all outcomes accumulated from prior parallel groups, so
/// it can resolve cross-task references (e.g. output-ref placeholders)
/// before the spawn. Same-group siblings are not yet completed when
/// `prepare_node` runs, but by construction they're independent of one
/// another.
///
/// On a non-recoverable error from `prepare_node` or `run_node`, or on
/// a `JoinError`, the in-flight tasks are aborted and the error is
/// returned immediately.
///
/// # Errors
///
/// Propagates the first `E` returned by `prepare_node` or `run_node`,
/// or a `JoinError` wrapped via the user-supplied `from_join_error`.
pub async fn walk_parallel_graph<T, O, E, P, F, Fut>(
    graph: &TaskGraph<T>,
    policy: WalkPolicy,
    mut prepare_node: P,
    run_node: F,
    from_join_error: impl Fn(tokio::task::JoinError) -> E + Send + Sync,
) -> Result<WalkSummary<O>, E>
where
    T: TaskNodeData + Clone + Send + Sync + 'static,
    O: WalkOutcome + Clone + Send + 'static,
    P: FnMut(GraphNode<T>, &HashMap<String, O>) -> Result<GraphNode<T>, E>,
    F: Fn(GraphNode<T>) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Result<O, E>> + Send + 'static,
    E: Send + 'static,
{
    let Ok(parallel_groups) = graph.get_parallel_groups() else {
        tracing::error!("parallel group computation failed");
        return Ok(WalkSummary {
            outcomes: Vec::new(),
            skipped: Vec::new(),
            failed: 0,
        });
    };

    let mut outcomes: Vec<(String, O)> = Vec::new();
    let mut outcomes_by_name: HashMap<String, O> = HashMap::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut tainted: HashSet<String> = HashSet::new();
    let mut failed = 0usize;

    'outer: for group in parallel_groups {
        // Filter out nodes whose dependencies have been tainted by an
        // earlier failure. Only relevant under continue_on_error — in
        // fail-fast mode we never reach the next group after a failure.
        let mut queue: std::collections::VecDeque<_> = group
            .into_iter()
            .filter_map(|node| {
                let failing_dep = node
                    .task
                    .dependency_names()
                    .find(|dep| tainted.contains(*dep))
                    .map(str::to_string);
                if let Some(dep) = failing_dep {
                    cuenv_events::emit_task_skipped!(
                        &node.name,
                        cuenv_events::SkipReason::DependencyFailed { dep }
                    );
                    tainted.insert(node.name.clone());
                    skipped.push(node.name.clone());
                    return None;
                }
                Some(node)
            })
            .collect();

        // Emit one Queued event per node that has to wait past the cap.
        if policy.max_parallel > 0 {
            for (position, node) in queue.iter().enumerate().skip(policy.max_parallel) {
                cuenv_events::emit_task_queued!(&node.name, position - policy.max_parallel);
            }
        }

        let mut join_set: JoinSet<Result<(String, O), E>> = JoinSet::new();

        while !queue.is_empty() || !join_set.is_empty() {
            while let Some(node) = queue.pop_front() {
                let prepared = match prepare_node(node, &outcomes_by_name) {
                    Ok(p) => p,
                    Err(err) => {
                        join_set.abort_all();
                        return Err(err);
                    }
                };
                let name = prepared.name.clone();
                let runner = run_node.clone();
                join_set.spawn(async move {
                    let outcome = runner(prepared).await?;
                    Ok((name, outcome))
                });
                if policy.max_parallel > 0 && join_set.len() >= policy.max_parallel {
                    break;
                }
            }

            let Some(joined) = join_set.join_next().await else {
                break;
            };
            match joined {
                Ok(Ok((name, outcome))) => {
                    let success = outcome.is_success();
                    if !success {
                        failed += 1;
                        tainted.insert(name.clone());
                    }
                    outcomes_by_name.insert(name.clone(), outcome.clone());
                    outcomes.push((name, outcome));
                    if !success && !policy.continue_on_error {
                        join_set.abort_all();
                        break 'outer;
                    }
                }
                Ok(Err(err)) => {
                    join_set.abort_all();
                    return Err(err);
                }
                Err(join_err) => {
                    join_set.abort_all();
                    return Err(from_join_error(join_err));
                }
            }
        }
    }

    Ok(WalkSummary {
        outcomes,
        skipped,
        failed,
    })
}

/// A `prepare_node` that does no work — pass this when the caller has
/// no cross-task references to resolve.
///
/// # Errors
///
/// Never returns an error; the signature matches `prepare_node`'s for
/// drop-in use.
pub fn passthrough_prepare<T, O, E>(
    node: GraphNode<T>,
    _outcomes: &HashMap<String, O>,
) -> Result<GraphNode<T>, E> {
    Ok(node)
}
