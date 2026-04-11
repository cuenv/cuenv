//! Top-level service controller for `cuenv up` orchestration.
//!
//! Walks a mixed task/service dependency graph, executing tasks to completion
//! and spawning service supervisors with readiness gating.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use cuenv_core::manifest::Service;
use cuenv_events::{emit_service_pending, emit_stdout};
use cuenv_task_graph::{NodeKind, TaskGraph};

use crate::session::SessionManager;
use crate::supervisor::{ReadinessOutcome, ServiceSupervisor, SupervisorConfig, SupervisorResult};

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
        let deps: Vec<String> = service.depends_on.iter().map(|d| d.name.clone()).collect();
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
        cuenv_core::tasks::TaskNode::Task(task) => {
            task.depends_on.iter().map(|d| d.name.clone()).collect()
        }
        cuenv_core::tasks::TaskNode::Group(group) => {
            group.depends_on.iter().map(|d| d.name.clone()).collect()
        }
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
    /// Returns an error if graph traversal fails or a service fails fatally
    /// during startup.
    pub async fn execute_up(
        &self,
        graph: &TaskGraph<MixedNode>,
        services: &HashMap<String, Service>,
        session: Arc<SessionManager>,
    ) -> crate::Result<()> {
        let parallel_groups = graph
            .get_parallel_groups()
            .map_err(cuenv_core::Error::from)?;
        let mut supervisor_handles: JoinSet<(String, SupervisorResult)> = JoinSet::new();
        let mut startup_failures: Vec<(String, String)> = Vec::new();

        'groups: for (group_idx, group) in parallel_groups.iter().enumerate() {
            debug!(group = group_idx, nodes = group.len(), "Processing group");

            let mut service_readiness: Vec<(String, watch::Receiver<ReadinessOutcome>)> =
                Vec::new();

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
                            let (readiness_tx, readiness_rx) =
                                watch::channel(ReadinessOutcome::Pending);
                            service_readiness.push((node.name.clone(), readiness_rx));

                            let supervisor = ServiceSupervisor::new(SupervisorConfig {
                                name: node.name.clone(),
                                service: service.clone(),
                                project_root: self.config.project_root.clone(),
                                session: Arc::clone(&session),
                            });

                            let shutdown = self.shutdown.clone();
                            let name = node.name.clone();
                            supervisor_handles.spawn(async move {
                                let result = supervisor.run(shutdown, readiness_tx).await;
                                (name, result)
                            });
                        }
                    }
                }
            }

            // Wait for all services in this group to become ready (or fail)
            for (name, mut readiness_rx) in service_readiness {
                debug!(service = %name, "Waiting for readiness");
                loop {
                    tokio::select! {
                        result = readiness_rx.changed() => {
                            if result.is_err() {
                                // Sender dropped — supervisor crashed before signaling
                                let msg = "supervisor exited before signaling readiness".to_string();
                                emit_stdout!(format!("Service '{name}' failed: {msg}"));
                                startup_failures.push((name.clone(), msg));
                                break;
                            }
                            match readiness_rx.borrow().clone() {
                                ReadinessOutcome::Pending => continue,
                                ReadinessOutcome::Ready => {
                                    debug!(service = %name, "Service ready");
                                    break;
                                }
                                ReadinessOutcome::Failed(msg) => {
                                    emit_stdout!(format!("Service '{name}' failed during startup: {msg}"));
                                    startup_failures.push((name.clone(), msg));
                                    break;
                                }
                            }
                        }
                        () = self.shutdown.cancelled() => {
                            debug!("Shutdown during readiness wait");
                            break 'groups;
                        }
                    }
                }
            }

            if self.shutdown.is_cancelled() || !startup_failures.is_empty() {
                break;
            }
        }

        if !startup_failures.is_empty() {
            // Cancel all supervisors on startup failure
            self.shutdown.cancel();

            // Drain remaining supervisor handles
            while let Some(result) = supervisor_handles.join_next().await {
                if let Ok((name, SupervisorResult::Failed(msg))) = result {
                    debug!(service = %name, error = %msg, "Supervisor failed during shutdown");
                }
            }

            let names: Vec<&str> = startup_failures.iter().map(|(n, _)| n.as_str()).collect();
            let details: Vec<String> = startup_failures
                .iter()
                .map(|(n, m)| format!("{n}: {m}"))
                .collect();
            return Err(crate::Error::ServiceFailed {
                name: names.join(", "),
                message: details.join("; "),
                help: Some("Check service logs with `cuenv logs <service>`".into()),
            });
        }

        if !self.shutdown.is_cancelled() {
            emit_stdout!("All services are up. Press Ctrl-C to stop.");
            // Monitor for runtime failures alongside shutdown signal
            loop {
                tokio::select! {
                    () = self.shutdown.cancelled() => break,
                    result = supervisor_handles.join_next() => {
                        match result {
                            Some(Ok((name, SupervisorResult::Failed(msg)))) => {
                                emit_stdout!(format!("Service '{name}' failed at runtime: {msg}"));
                                self.shutdown.cancel();
                                break;
                            }
                            Some(Ok((_, SupervisorResult::Stopped))) => {}
                            Some(Err(e)) => {
                                emit_stdout!(format!("Service supervisor panicked: {e}"));
                                self.shutdown.cancel();
                                break;
                            }
                            None => break, // All supervisors exited
                        }
                    }
                }
            }
        }

        emit_stdout!("Shutting down services...");

        // Wait for remaining supervisors to finish
        let mut runtime_failures: Vec<(String, String)> = Vec::new();
        while let Some(result) = supervisor_handles.join_next().await {
            match result {
                Ok((name, SupervisorResult::Stopped)) => {
                    debug!(service = %name, "Supervisor stopped");
                }
                Ok((name, SupervisorResult::Failed(msg))) => {
                    debug!(service = %name, error = %msg, "Supervisor failed");
                    runtime_failures.push((name, msg));
                }
                Err(e) => {
                    debug!(error = %e, "Supervisor task panicked");
                    runtime_failures
                        .push(("unknown".to_string(), format!("supervisor panicked: {e}")));
                }
            }
        }

        if !runtime_failures.is_empty() {
            let names: Vec<&str> = runtime_failures.iter().map(|(n, _)| n.as_str()).collect();
            let details: Vec<String> = runtime_failures
                .iter()
                .map(|(n, m)| format!("{n}: {m}"))
                .collect();
            return Err(crate::Error::ServiceFailed {
                name: names.join(", "),
                message: details.join("; "),
                help: None,
            });
        }

        Ok(())
    }
}
