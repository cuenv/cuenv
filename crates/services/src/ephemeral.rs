//! Ephemeral task-scoped service lifecycle management.
//!
//! Starts the services a task declares in `dependsOn` (along with their
//! transitive service dependencies), waits for all readiness probes to pass,
//! and tears them down cleanly when the task completes.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use cuenv_core::manifest::Service;
use cuenv_events::emit_service_pending;
use cuenv_task_graph::NodeKind;

use crate::controller::build_service_graph;
use crate::session::SessionManager;
use crate::supervisor::{ReadinessOutcome, ServiceSupervisor, SupervisorConfig, SupervisorResult};

static EPHEMERAL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Request describing which ephemeral services to start for a task run.
pub struct EphemeralServicesRequest<'request> {
    /// Project root directory where services execute.
    pub project_root: PathBuf,
    /// Project name for session metadata.
    pub project_name: &'request str,
    /// All services declared in the project.
    pub services: &'request HashMap<String, Service>,
    /// Names of services directly required by the task.
    pub requested: &'request [String],
}

/// Guard holding running ephemeral services for a single task execution.
///
/// Call [`Self::shutdown`] after the task finishes for clean asynchronous
/// teardown. Dropping the guard without calling `shutdown` still cancels the
/// shutdown token and removes the temporary session directory so services do
/// not leak.
pub struct EphemeralServicesGuard {
    shutdown: CancellationToken,
    handles: JoinSet<(String, SupervisorResult)>,
    session_dir: PathBuf,
}

impl EphemeralServicesGuard {
    /// Gracefully stop all ephemeral services and await supervisor teardown.
    pub async fn shutdown(mut self) {
        self.shutdown.cancel();
        while let Some(result) = self.handles.join_next().await {
            match result {
                Ok((name, SupervisorResult::Stopped)) => {
                    debug!(service = %name, "Ephemeral service stopped");
                }
                Ok((name, SupervisorResult::Failed(msg))) => {
                    debug!(service = %name, error = %msg, "Ephemeral service failed during shutdown");
                }
                Err(error) => {
                    debug!(%error, "Ephemeral service supervisor panicked during shutdown");
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.session_dir);
    }
}

impl Drop for EphemeralServicesGuard {
    fn drop(&mut self) {
        self.shutdown.cancel();
        let _ = std::fs::remove_dir_all(&self.session_dir);
    }
}

/// Start the requested services (and their transitive service dependencies),
/// waiting until every service passes its readiness probe.
///
/// # Errors
///
/// Returns an error if a requested service is not declared, the service graph
/// has a cycle, or any service fails to reach readiness.
pub async fn start_ephemeral_services(
    request: EphemeralServicesRequest<'_>,
) -> crate::Result<EphemeralServicesGuard> {
    let EphemeralServicesRequest {
        project_root,
        project_name,
        services,
        requested,
    } = request;

    let selected = collect_transitive_services(services, requested)?;
    let graph = build_service_graph(&selected)?;
    let parallel_groups = graph.parallel_groups().map_err(cuenv_core::Error::from)?;

    let id = EPHEMERAL_COUNTER.fetch_add(1, Ordering::Relaxed);
    let session_dir = std::env::temp_dir().join(format!(
        "cuenv-ephemeral-services-{}-{id}",
        std::process::id()
    ));
    let session = Arc::new(SessionManager::create_ephemeral(
        session_dir.clone(),
        &project_root,
        project_name,
    )?);

    let shutdown = CancellationToken::new();
    let mut handles: JoinSet<(String, SupervisorResult)> = JoinSet::new();
    let mut startup_failures: Vec<(String, String)> = Vec::new();

    'groups: for group in &parallel_groups {
        let mut service_readiness: Vec<(String, watch::Receiver<ReadinessOutcome>)> = Vec::new();

        for node in group {
            if node.kind != NodeKind::Service {
                continue;
            }
            emit_service_pending!(&node.name);

            if let Some(service) = selected.get(&node.name) {
                let (readiness_tx, readiness_rx) = watch::channel(ReadinessOutcome::Pending);
                service_readiness.push((node.name.clone(), readiness_rx));

                let supervisor = ServiceSupervisor::new(SupervisorConfig {
                    name: node.name.clone(),
                    service: service.clone(),
                    project_root: project_root.clone(),
                    session: Arc::clone(&session),
                });

                let token = shutdown.clone();
                let name = node.name.clone();
                handles.spawn(async move {
                    let result = supervisor.run(token, readiness_tx).await;
                    (name, result)
                });
            }
        }

        for (name, mut readiness_rx) in service_readiness {
            loop {
                tokio::select! {
                    result = readiness_rx.changed() => {
                        if result.is_err() {
                            startup_failures.push((
                                name.clone(),
                                "supervisor exited before signaling readiness".to_string(),
                            ));
                            break;
                        }
                        let outcome = readiness_rx.borrow().clone();
                        match outcome {
                            ReadinessOutcome::Pending => {}
                            ReadinessOutcome::Ready => {
                                debug!(service = %name, "Ephemeral service ready");
                                break;
                            }
                            ReadinessOutcome::Failed(msg) => {
                                startup_failures.push((name.clone(), msg));
                                break;
                            }
                        }
                    }
                    () = shutdown.cancelled() => {
                        break 'groups;
                    }
                }
            }
        }

        if !startup_failures.is_empty() {
            break;
        }
    }

    let guard = EphemeralServicesGuard {
        shutdown,
        handles,
        session_dir,
    };

    if !startup_failures.is_empty() {
        guard.shutdown().await;
        let names: Vec<&str> = startup_failures.iter().map(|(n, _)| n.as_str()).collect();
        let details: Vec<String> = startup_failures
            .iter()
            .map(|(n, m)| format!("{n}: {m}"))
            .collect();
        return Err(crate::Error::ServiceFailed {
            name: names.join(", "),
            message: details.join("; "),
            help: Some("Verify the service command and readiness probe".into()),
        });
    }

    Ok(guard)
}

fn collect_transitive_services(
    all_services: &HashMap<String, Service>,
    requested: &[String],
) -> crate::Result<HashMap<String, Service>> {
    let mut selected = HashMap::new();
    let mut queue: Vec<String> = requested.to_vec();
    let mut visited: HashSet<String> = HashSet::new();

    while let Some(name) = queue.pop() {
        if !visited.insert(name.clone()) {
            continue;
        }
        let service = all_services.get(&name).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "task depends on unknown service '{name}' (not declared in project services)"
            ))
        })?;
        for dep in &service.depends_on {
            if all_services.contains_key(&dep.name) {
                queue.push(dep.name.clone());
            }
        }
        selected.insert(name, service.clone());
    }

    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_service(sleep_secs: u32) -> Service {
        serde_json::from_value(serde_json::json!({
            "type": "service",
            "entrypoint": {
                "command": "sleep",
                "args": [sleep_secs.to_string()]
            },
            "readiness": {
                "kind": "delay",
                "delay": "20ms"
            }
        }))
        .expect("valid service fixture")
    }

    #[tokio::test]
    async fn starts_and_shuts_down_ephemeral_service() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut services = HashMap::new();
        services.insert("worker".to_string(), dummy_service(30));

        let requested = vec!["worker".to_string()];
        let guard = start_ephemeral_services(EphemeralServicesRequest {
            project_root: dir.path().to_path_buf(),
            project_name: "test-project",
            services: &services,
            requested: &requested,
        })
        .await
        .expect("ephemeral service should start");

        let session_dir = guard.session_dir.clone();
        assert!(session_dir.exists());
        guard.shutdown().await;
        assert!(!session_dir.exists());
    }

    #[tokio::test]
    async fn rejects_unknown_service() {
        let dir = tempfile::tempdir().expect("tempdir");
        let services = HashMap::new();
        let requested = vec!["missing".to_string()];
        let err = start_ephemeral_services(EphemeralServicesRequest {
            project_root: dir.path().to_path_buf(),
            project_name: "test-project",
            services: &services,
            requested: &requested,
        })
        .await;
        assert!(err.is_err());
    }
}
