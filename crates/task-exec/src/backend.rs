//! Pluggable task execution backends and ephemeral host service hooks.
//!
//! External backends (such as the Dagger Engine backend in `cuenv-dagger`)
//! implement [`TaskBackend`] so [`crate::TaskExecutor`] can delegate container
//! and Dagger-runtime tasks while keeping DAG scheduling, retries, timeouts,
//! and event emission unified.
//!
//! Host-executed tasks that declare `#Service` entries in `dependsOn` use
//! [`EphemeralServiceManager`] to start those services for the duration of the
//! task and tear them down cleanly on completion or failure.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use cuenv_core::environment::Environment;
use cuenv_core::manifest::{Runtime, Service};
use cuenv_core::{OutputCapture, Result};

use crate::Task;
pub use crate::process::TaskAttempt;

/// Pending result of [`TaskBackend::execute_attempt`].
pub type BackendAttemptFuture<'a> = Pin<Box<dyn Future<Output = Result<TaskAttempt>> + Send + 'a>>;

/// Pending result of [`EphemeralServiceManager::start`].
pub type EphemeralServiceStartFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn EphemeralServiceHandle>>> + Send + 'a>>;

/// Pending completion of [`EphemeralServiceHandle::shutdown`].
pub type EphemeralServiceShutdownFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// All inputs required to run a single attempt of a task on an external backend.
#[derive(Debug)]
pub struct BackendAttemptRequest<'request> {
    /// Fully qualified or local task name (used in events and logs).
    pub name: &'request str,
    /// Task definition.
    pub task: &'request Task,
    /// Resolved environment for this task (including secret markers).
    pub environment: &'request Environment,
    /// Project root directory (`env.cue` directory).
    pub project_root: &'request Path,
    /// Resolved working directory on the host.
    pub workdir: &'request Path,
    /// Effective runtime (task runtime overriding project runtime, if any).
    pub runtime: Option<&'request Runtime>,
    /// All services declared in the project (for `dependsOn` service bindings).
    pub services: &'request HashMap<String, Service>,
    /// Optional per-attempt timeout.
    pub timeout: Option<Duration>,
    /// Whether stdout/stderr should be captured or streamed.
    pub capture_output: OutputCapture,
}

/// Pluggable execution backend for tasks (for example, Dagger 1.0).
pub trait TaskBackend: std::fmt::Debug + Send + Sync {
    /// Human-readable backend name (e.g. `"dagger"`).
    fn name(&self) -> &'static str;

    /// Whether this backend manages its own caching and input/output staging.
    ///
    /// When `true`, [`crate::TaskExecutor`] bypasses `cuenv-cas` and local
    /// `exec_root` materialization and emits
    /// `CacheSkipReason::ExternalBackend`.
    fn manages_own_cache(&self) -> bool {
        true
    }

    /// Whether this backend should execute the given task and effective runtime.
    fn handles_task(&self, task: &Task, runtime: Option<&Runtime>) -> bool;

    /// Execute a single attempt of a task.
    fn execute_attempt<'a>(
        &'a self,
        request: BackendAttemptRequest<'a>,
    ) -> BackendAttemptFuture<'a>;
}

/// Request to start ephemeral services for a host-executed task.
#[derive(Debug)]
pub struct EphemeralServiceStart<'request> {
    /// Project root directory where services run.
    pub project_root: &'request Path,
    /// Project name for session metadata.
    pub project_name: &'request str,
    /// All services declared in the project.
    pub services: &'request HashMap<String, Service>,
    /// Service names directly required by the task.
    pub requested: &'request [String],
}

/// Handle to running ephemeral services that must be shut down after the task.
pub trait EphemeralServiceHandle: Send + Sync {
    /// Gracefully stop all started ephemeral services and clean up state.
    fn shutdown(self: Box<Self>) -> EphemeralServiceShutdownFuture<'static>;
}

/// Starts task-scoped ephemeral services for host-executed tasks.
pub trait EphemeralServiceManager: std::fmt::Debug + Send + Sync {
    /// Start the requested services and wait for their readiness probes.
    fn start<'a>(&'a self, request: EphemeralServiceStart<'a>) -> EphemeralServiceStartFuture<'a>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::{ExecutorConfig, TaskDependency, TaskExecutor, TaskResult};

    #[derive(Debug)]
    struct MockBackend {
        calls: Arc<AtomicUsize>,
        fail_first: bool,
    }

    impl TaskBackend for MockBackend {
        fn name(&self) -> &'static str {
            "mock"
        }

        fn handles_task(&self, _task: &Task, runtime: Option<&Runtime>) -> bool {
            matches!(runtime, Some(Runtime::Dagger(_) | Runtime::Container(_)))
        }

        fn execute_attempt<'a>(
            &'a self,
            request: BackendAttemptRequest<'a>,
        ) -> BackendAttemptFuture<'a> {
            let calls = Arc::clone(&self.calls);
            let fail_first = self.fail_first;
            let name = request.name.to_string();
            Box::pin(async move {
                let attempt = calls.fetch_add(1, Ordering::SeqCst);
                if fail_first && attempt == 0 {
                    return Ok(TaskAttempt::Completed(TaskResult {
                        name,
                        exit_code: Some(1),
                        stdout: String::new(),
                        stderr: "transient".to_string(),
                        success: false,
                    }));
                }
                Ok(TaskAttempt::Completed(TaskResult {
                    name,
                    exit_code: Some(0),
                    stdout: "ok".to_string(),
                    stderr: String::new(),
                    success: true,
                }))
            })
        }
    }

    #[derive(Debug)]
    struct MockServiceHandle {
        shutdowns: Arc<AtomicUsize>,
    }

    impl EphemeralServiceHandle for MockServiceHandle {
        fn shutdown(self: Box<Self>) -> EphemeralServiceShutdownFuture<'static> {
            let shutdowns = Arc::clone(&self.shutdowns);
            Box::pin(async move {
                shutdowns.fetch_add(1, Ordering::SeqCst);
            })
        }
    }

    #[derive(Debug)]
    struct MockServiceManager {
        starts: Arc<AtomicUsize>,
        shutdowns: Arc<AtomicUsize>,
    }

    impl EphemeralServiceManager for MockServiceManager {
        fn start<'a>(
            &'a self,
            _request: EphemeralServiceStart<'a>,
        ) -> EphemeralServiceStartFuture<'a> {
            let starts = Arc::clone(&self.starts);
            let shutdowns = Arc::clone(&self.shutdowns);
            Box::pin(async move {
                starts.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(MockServiceHandle { shutdowns }) as Box<dyn EphemeralServiceHandle>)
            })
        }
    }

    #[tokio::test]
    async fn routes_dagger_runtime_task_to_backend_with_retries() {
        let calls = Arc::new(AtomicUsize::new(0));
        let backend = Arc::new(MockBackend {
            calls: Arc::clone(&calls),
            fail_first: true,
        });
        let dir = tempfile::tempdir().expect("tempdir");
        let executor = TaskExecutor::new(ExecutorConfig {
            project_root: dir.path().to_path_buf(),
            backend: Some(backend),
            ..ExecutorConfig::default()
        });

        let mut task: Task = serde_json::from_value(serde_json::json!({
            "command": "cargo",
            "args": ["test"],
            "runtime": {
                "type": "dagger",
                "image": "rust:1.88"
            },
            "retry": {
                "attempts": 1
            }
        }))
        .expect("valid task");
        task.project_root = Some(dir.path().to_path_buf());

        let result = executor
            .execute_task("test", &task)
            .await
            .expect("backend execution should succeed");
        assert!(result.success);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn host_task_starts_and_shuts_down_ephemeral_services_even_on_failure() {
        let starts = Arc::new(AtomicUsize::new(0));
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let manager = Arc::new(MockServiceManager {
            starts: Arc::clone(&starts),
            shutdowns: Arc::clone(&shutdowns),
        });
        let dir = tempfile::tempdir().expect("tempdir");
        let executor = TaskExecutor::new(ExecutorConfig {
            project_root: dir.path().to_path_buf(),
            service_manager: Some(manager),
            ..ExecutorConfig::default()
        });

        let mut task = Task {
            command: "false".to_string(),
            ..Task::default()
        };
        task.add_typed_dependency(TaskDependency::from_service_name("postgres"));
        task.project_root = Some(dir.path().to_path_buf());

        let result = executor
            .execute_task("integration", &task)
            .await
            .expect("task should return failure result");
        assert!(!result.success);
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    }
}
