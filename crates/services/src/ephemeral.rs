//! Ephemeral task-scoped service lifecycle management.
//!
//! Starts the services a task declares in `dependsOn` (along with their
//! transitive service dependencies), waits for all readiness probes to pass,
//! and tears them down cleanly when the task completes.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::runtime::Handle;
use tokio::sync::{OwnedMutexGuard, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use cuenv_core::manifest::{Runtime, Service};
use cuenv_core::tasks::DependencyKind;
use cuenv_events::emit_service_pending;
use cuenv_task_graph::NodeKind;

use crate::controller::build_service_graph;
use crate::host_resources::lock_service_resources;
use crate::session::SessionManager;
use crate::supervisor::{ReadinessOutcome, ServiceSupervisor, SupervisorConfig, SupervisorResult};

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
/// teardown. Dropping the guard starts teardown in the background, including
/// when the task is cancelled during service startup.
pub struct EphemeralServicesGuard {
    shutdown: CancellationToken,
    runtime: Handle,
    handles: JoinSet<(String, SupervisorResult)>,
    directory: Option<tempfile::TempDir>,
    resources: Vec<OwnedMutexGuard<()>>,
}

impl EphemeralServicesGuard {
    /// Gracefully stop all ephemeral services and await supervisor teardown.
    pub async fn shutdown(mut self) {
        if let Some(cleanup) = self.start_shutdown()
            && let Err(error) = cleanup.await
        {
            warn!(%error, "Ephemeral service session cleanup failed");
        }
    }

    fn start_shutdown(&mut self) -> Option<JoinHandle<()>> {
        self.shutdown.cancel();
        let directory = self.directory.take()?;
        let handles = std::mem::take(&mut self.handles);
        let resources = std::mem::take(&mut self.resources);
        // The cleanup task owns the supervisors and directory even when the
        // future awaiting shutdown is itself cancelled.
        Some(
            self.runtime
                .spawn(finish_shutdown(handles, directory, resources)),
        )
    }
}

impl Drop for EphemeralServicesGuard {
    fn drop(&mut self) {
        drop(self.start_shutdown());
    }
}

async fn finish_shutdown(
    mut handles: JoinSet<(String, SupervisorResult)>,
    directory: tempfile::TempDir,
    resources: Vec<OwnedMutexGuard<()>>,
) {
    while let Some(result) = handles.join_next().await {
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
    if let Err(error) = directory.close() {
        warn!(%error, "Failed to remove ephemeral service session directory");
    }
    drop(resources);
}

/// Start the requested services (and their transitive service dependencies),
/// waiting until every service passes its readiness probe.
///
/// # Errors
///
/// Returns an error if a requested service is not declared, the service graph
/// has a cycle, a service requires an unsupported runtime or prerequisite,
/// or any service fails to reach readiness.
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
    let resources = lock_service_resources(&project_root, &selected).await?;

    let directory = tempfile::Builder::new()
        .prefix("cuenv-ephemeral-services-")
        .tempdir()?;
    let session = Arc::new(SessionManager::create_ephemeral(
        directory.path().to_path_buf(),
        &project_root,
        project_name,
    )?);

    let mut guard = EphemeralServicesGuard {
        shutdown: CancellationToken::new(),
        runtime: Handle::current(),
        handles: JoinSet::new(),
        directory: Some(directory),
        resources,
    };

    for group in &parallel_groups {
        let mut service_readiness = JoinSet::new();

        for node in group {
            if node.kind != NodeKind::Service {
                continue;
            }
            emit_service_pending!(&node.name);

            if let Some(service) = selected.get(&node.name) {
                let (readiness_tx, mut readiness_rx) = watch::channel(ReadinessOutcome::Pending);
                let name = node.name.clone();
                service_readiness.spawn(async move {
                    let result = match readiness_rx
                        .wait_for(|outcome| *outcome != ReadinessOutcome::Pending)
                        .await
                        .as_deref()
                    {
                        Ok(ReadinessOutcome::Failed(message)) => Err(message.clone()),
                        Ok(_) => Ok(()),
                        Err(_) => Err("supervisor exited before signaling readiness".to_string()),
                    };
                    (name, result)
                });

                let supervisor = ServiceSupervisor::new(SupervisorConfig {
                    name: node.name.clone(),
                    service: service.clone(),
                    project_root: project_root.clone(),
                    session: Arc::clone(&session),
                });

                let token = guard.shutdown.clone();
                let name = node.name.clone();
                guard.handles.spawn(async move {
                    let result = supervisor.run(token, readiness_tx).await;
                    (name, result)
                });
            }
        }

        while let Some(readiness) = service_readiness.join_next().await {
            match readiness {
                Ok((name, Ok(()))) => {
                    debug!(service = %name, "Ephemeral service ready");
                }
                Ok((name, Err(message))) => {
                    guard.shutdown().await;
                    return Err(crate::Error::ServiceFailed {
                        name,
                        message,
                        help: Some("Verify the service command and readiness probe".into()),
                    });
                }
                Err(error) => {
                    guard.shutdown().await;
                    return Err(crate::Error::session(format!(
                        "readiness waiter failed: {error}"
                    )));
                }
            }
        }
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
        // Session state, logs and control paths use the service name as a filename.
        if Path::new(&name).file_name() != Some(OsStr::new(&name)) {
            return Err(cuenv_core::Error::configuration(format!(
                "host ephemeral service name '{name}' must be a single path component"
            ))
            .into());
        }
        if matches!(
            service.runtime,
            Some(Runtime::Container(_) | Runtime::Dagger(_))
        ) {
            return Err(cuenv_core::Error::configuration(format!(
                "service '{name}' requires a container runtime; use the Dagger backend"
            ))
            .into());
        }
        for dep in &service.depends_on {
            let prerequisite = match dep.kind() {
                DependencyKind::Service => {
                    queue.push(dep.name.clone());
                    continue;
                }
                DependencyKind::Task => "task",
                DependencyKind::Image => "image",
            };
            return Err(cuenv_core::Error::configuration(format!(
                "host ephemeral service '{name}' depends on {prerequisite} '{}'; only service prerequisites are supported",
                dep.name
            ))
            .into());
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

        let session_dir = guard
            .directory
            .as_ref()
            .expect("active session")
            .path()
            .to_path_buf();
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

    #[test]
    fn rejects_task_prerequisite_with_same_name_as_service() -> Result<(), serde_json::Error> {
        let worker = serde_json::from_value(serde_json::json!({
            "type": "service",
            "entrypoint": {"command": "sleep", "args": ["30"]},
            "dependsOn": [{"_name": "prepare", "kind": "task"}]
        }))?;
        let services = HashMap::from([
            ("worker".to_string(), worker),
            ("prepare".to_string(), dummy_service(30)),
        ]);
        let result = collect_transitive_services(&services, &["worker".to_string()]);
        assert!(result.is_err_and(|error| error.to_string().contains("depends on task 'prepare'")));
        Ok(())
    }

    #[test]
    fn rejects_service_names_outside_the_session_directory() {
        let name = "../worker".to_string();
        let services = HashMap::from([(name.clone(), dummy_service(30))]);
        let result = collect_transitive_services(&services, &[name]);
        assert!(result.is_err_and(|error| error.to_string().contains("a single path component")));
    }

    #[cfg(unix)]
    fn service_with_shutdown_marker(
        readiness: &serde_json::Value,
    ) -> Result<Service, serde_json::Error> {
        serde_json::from_value(serde_json::json!({
            "type": "service",
            "entrypoint": {
                "command": "sh",
                "args": ["-c", "trap 'touch stopped; exit' TERM; touch started; echo running; while :; do sleep 0.05; done"]
            },
            "readiness": readiness
        }))
    }

    #[cfg(unix)]
    async fn wait_until(
        condition: impl Fn() -> bool + Send,
    ) -> Result<(), tokio::time::error::Elapsed> {
        use std::time::Duration;

        tokio::time::timeout(Duration::from_secs(5), async move {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_guard_gracefully_stops_service_before_removing_session()
    -> Result<(), Box<dyn std::error::Error>> {
        let project = tempfile::tempdir()?;
        let service = service_with_shutdown_marker(&serde_json::json!({
            "kind": "log", "pattern": "running", "interval": "20ms"
        }))?;
        let services = HashMap::from([("worker".to_string(), service)]);
        let guard = start_ephemeral_services(EphemeralServicesRequest {
            project_root: project.path().to_path_buf(),
            project_name: "test-project",
            services: &services,
            requested: &["worker".to_string()],
        })
        .await?;
        let session_dir = guard
            .directory
            .as_ref()
            .ok_or_else(|| crate::Error::session("missing test session directory"))?
            .path()
            .to_path_buf();
        assert!(session_dir.exists());

        drop(guard);
        wait_until(|| !session_dir.exists()).await?;
        assert!(project.path().join("stopped").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_startup_stops_service_during_readiness()
    -> Result<(), Box<dyn std::error::Error>> {
        let project = tempfile::tempdir()?;
        let service = service_with_shutdown_marker(&serde_json::json!({
            "kind": "delay", "delay": "1h"
        }))?;
        let services = HashMap::from([("worker".to_string(), service)]);
        let requested = ["worker".to_string()];
        let started = project.path().join("started");
        let mut startup = Box::pin(start_ephemeral_services(EphemeralServicesRequest {
            project_root: project.path().to_path_buf(),
            project_name: "test-project",
            services: &services,
            requested: &requested,
        }));

        tokio::select! {
            result = wait_until(|| started.exists()) => result?,
            _ = &mut startup => return Err("service finished startup before cancellation".into()),
        }
        drop(startup);

        wait_until(|| project.path().join("stopped").exists()).await?;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shared_service_waits_for_previous_guard_teardown()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::time::Duration;

        let project = tempfile::tempdir()?;
        let service = serde_json::from_value(serde_json::json!({
            "type": "service",
            "entrypoint": {
                "command": "sh",
                "args": ["-c", "trap 'sleep 0.2; echo stopped >> lifecycle; exit' TERM; echo started >> lifecycle; echo running; while :; do sleep 0.05; done"]
            },
            "readiness": {"kind": "log", "pattern": "running", "interval": "20ms"}
        }))?;
        let services = HashMap::from([("worker".to_string(), service)]);
        let requested = ["worker".to_string()];
        let request = || EphemeralServicesRequest {
            project_root: project.path().to_path_buf(),
            project_name: "test-project",
            services: &services,
            requested: &requested,
        };

        let first = start_ephemeral_services(request()).await?;
        let mut second = Box::pin(start_ephemeral_services(request()));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut second)
                .await
                .is_err()
        );
        drop(first);
        let second = tokio::time::timeout(Duration::from_secs(5), second).await??;
        assert_eq!(
            std::fs::read_to_string(project.path().join("lifecycle"))?,
            "started\nstopped\nstarted\n"
        );
        second.shutdown().await;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn runtime_shutdown_stops_service_process_group() -> Result<(), Box<dyn std::error::Error>> {
        use std::time::Duration;

        let project = tempfile::tempdir()?;
        let service = serde_json::from_value(serde_json::json!({
            "type": "service",
            "entrypoint": {
                "command": "sh",
                "args": ["-c", "sleep 10 & printf '%s\\n' \"$!\" > child.pid; echo running; wait"]
            },
            "readiness": {"kind": "log", "pattern": "running", "interval": "20ms"}
        }))?;
        let services = HashMap::from([("worker".to_string(), service)]);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let guard = runtime.block_on(start_ephemeral_services(EphemeralServicesRequest {
            project_root: project.path().to_path_buf(),
            project_name: "test-project",
            services: &services,
            requested: &["worker".to_string()],
        }))?;
        let pid = std::fs::read_to_string(project.path().join("child.pid"))?
            .trim()
            .parse()?;
        assert!(service_child_is_running(pid));

        // A current-thread runtime cannot poll background teardown between
        // dropping the guard and shutting down the runtime.
        drop(guard);
        drop(runtime);

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while service_child_is_running(pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!service_child_is_running(pid));
        Ok(())
    }

    #[cfg(unix)]
    fn service_child_is_running(pid: u32) -> bool {
        #[cfg(target_os = "linux")]
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            && let Some((_, state)) = stat.rsplit_once(") ")
            && state.starts_with('Z')
        {
            // A container's init can leave a terminated orphan unreaped.
            return false;
        }
        crate::session::is_pid_alive(pid)
    }
}
