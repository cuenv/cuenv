//! Top-level service controller for `cuenv up` orchestration.
//!
//! Walks a mixed task/service dependency graph, executing tasks to completion
//! and spawning service supervisors with readiness gating.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use cuenv_core::manifest::Service;
use cuenv_events::{emit_service_pending, emit_stdout};
use cuenv_task_graph::{NodeKind, TaskGraph};

use crate::session::SessionManager;
use crate::supervisor::{ServiceSupervisor, SupervisorResult};

/// Configuration for the service controller.
pub struct ControllerConfig {
    /// Project root directory.
    pub project_root: PathBuf,
}

/// Mixed node data for the combined task/service graph.
///
/// Implements `TaskNodeData` to participate in the existing graph algorithms.
#[derive(Debug, Clone)]
pub struct MixedNode {
    /// Dependencies by name.
    pub dependencies: Vec<String>,
}

impl cuenv_task_graph::TaskNodeData for MixedNode {
    fn dependency_names(&self) -> impl Iterator<Item = &str> {
        self.dependencies.iter().map(String::as_str)
    }

    fn add_dependency(&mut self, dep: String) {
        if !self.dependencies.contains(&dep) {
            self.dependencies.push(dep);
        }
    }
}

/// Build a mixed task/service graph from project configuration.
///
/// # Errors
///
/// Returns an error if graph construction or cycle detection fails.
pub fn build_mixed_graph(
    tasks: &HashMap<String, cuenv_core::tasks::TaskNode>,
    services: &HashMap<String, Service>,
) -> crate::Result<TaskGraph<MixedNode>> {
    let mut graph = TaskGraph::new();

    // Add task nodes
    for (name, node) in tasks {
        let deps = collect_task_deps(node);
        let mixed = MixedNode { dependencies: deps };
        graph.add_task(name, mixed)?;
    }

    // Add service nodes
    for (name, service) in services {
        let deps: Vec<String> = service
            .depends_on
            .iter()
            .map(|d| d.name.clone())
            .collect();
        let mixed = MixedNode { dependencies: deps };
        graph.add_service(name, mixed)?;
    }

    // Resolve dependency edges
    graph.add_dependency_edges()?;

    Ok(graph)
}

/// Collect dependency names from a task node.
fn collect_task_deps(node: &cuenv_core::tasks::TaskNode) -> Vec<String> {
    match node {
        cuenv_core::tasks::TaskNode::Task(task) => task
            .depends_on
            .iter()
            .map(|d| d.name.clone())
            .collect(),
        cuenv_core::tasks::TaskNode::Group(group) => group
            .depends_on
            .iter()
            .map(|d| d.name.clone())
            .collect(),
        cuenv_core::tasks::TaskNode::Sequence(_) => Vec::new(),
    }
}

/// The main service controller.
pub struct ServiceController {
    config: ControllerConfig,
    shutdown: CancellationToken,
}

impl ServiceController {
    /// Create a new service controller.
    #[must_use]
    pub fn new(config: ControllerConfig, shutdown: CancellationToken) -> Self {
        Self { config, shutdown }
    }

    /// Bring up all services in the graph.
    ///
    /// Walks parallel groups in topological order:
    /// - Task nodes are noted as satisfied (actual task execution is deferred to caller)
    /// - Service nodes spawn supervisors and wait for readiness
    ///
    /// After all groups are processed, blocks until the shutdown token fires.
    ///
    /// # Errors
    ///
    /// Returns an error if graph traversal fails or a service fails fatally.
    pub async fn execute_up(
        &self,
        graph: &TaskGraph<MixedNode>,
        services: &HashMap<String, Service>,
        session: Arc<SessionManager>,
    ) -> crate::Result<()> {
        let parallel_groups = graph.get_parallel_groups().map_err(cuenv_core::Error::from)?;
        let mut supervisor_handles: JoinSet<(String, SupervisorResult)> = JoinSet::new();

        for (group_idx, group) in parallel_groups.iter().enumerate() {
            debug!(group = group_idx, nodes = group.len(), "Processing group");

            let mut service_readiness: Vec<(String, Arc<Notify>)> = Vec::new();

            for node in group {
                match node.kind {
                    NodeKind::Task => {
                        // For now, tasks in the service graph are considered
                        // pre-satisfied. Full integration with TaskExecutor
                        // will be wired in Phase 4 (CLI commands).
                        debug!(task = %node.name, "Task node (pre-satisfied)");
                    }
                    NodeKind::Service => {
                        emit_service_pending!(&node.name);

                        if let Some(service) = services.get(&node.name) {
                            let ready_notify = Arc::new(Notify::new());
                            service_readiness.push((
                                node.name.clone(),
                                Arc::clone(&ready_notify),
                            ));

                            let supervisor = ServiceSupervisor::new(
                                node.name.clone(),
                                service.clone(),
                                self.config.project_root.clone(),
                                Arc::clone(&session),
                            );

                            let shutdown = self.shutdown.clone();
                            let name = node.name.clone();
                            supervisor_handles.spawn(async move {
                                let result = supervisor.run(shutdown, ready_notify).await;
                                (name, result)
                            });
                        }
                    }
                }
            }

            // Wait for all services in this group to become ready
            for (name, notify) in &service_readiness {
                debug!(service = %name, "Waiting for readiness");
                tokio::select! {
                    () = notify.notified() => {
                        debug!(service = %name, "Service ready");
                    }
                    () = self.shutdown.cancelled() => {
                        debug!("Shutdown during readiness wait");
                        break;
                    }
                }
            }

            if self.shutdown.is_cancelled() {
                break;
            }
        }

        if !self.shutdown.is_cancelled() {
            emit_stdout!("All services are up. Press Ctrl-C to stop.");
            // Block until shutdown
            self.shutdown.cancelled().await;
        }

        emit_stdout!("Shutting down services...");

        // Wait for all supervisors to finish (they respond to shutdown token)
        while let Some(result) = supervisor_handles.join_next().await {
            match result {
                Ok((name, SupervisorResult::Stopped)) => {
                    debug!(service = %name, "Supervisor stopped");
                }
                Ok((name, SupervisorResult::Failed(msg))) => {
                    debug!(service = %name, error = %msg, "Supervisor failed");
                }
                Err(e) => {
                    debug!(error = %e, "Supervisor task panicked");
                }
            }
        }

        Ok(())
    }
}
