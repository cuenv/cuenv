//! Implementation of the `cuenv down` command.
//!
//! The command operates on the persisted service session created by `cuenv up`.
//! It requests whole-session shutdown from the controller or named-service stop
//! requests from individual supervisors.

use std::path::Path;

use cuenv_events::emit_stdout;
use cuenv_services::session::{SessionManager, ShutdownRequestOutcome};

/// Options for the `cuenv down` command.
pub struct DownOptions {
    /// Path to directory containing CUE files.
    pub path: String,
    /// CUE package name to evaluate.
    pub package: String,
    /// Optional list of service names to bring down (empty = all).
    pub services: Vec<String>,
}

/// Execute the `cuenv down` command.
///
/// Whole-session shutdown is supported by signalling the active `cuenv up`
/// controller. Named-service shutdown queues stop requests for running
/// supervisors in the persisted session state.
///
/// # Errors
///
/// Returns an error if no session exists, session state cannot be read, a
/// controller cannot be signalled, or a named service cannot be found.
pub fn execute_down(options: &DownOptions) -> cuenv_core::Result<String> {
    let session = SessionManager::load(Path::new(&options.path))
        .map_err(|e| cuenv_core::Error::execution(format!("Failed to load session: {e}")))?;

    emit_stdout!(format!(
        "cuenv down: requesting shutdown for services in {} (package: {})",
        options.path, options.package
    ));

    if !options.services.is_empty() {
        return request_named_service_shutdown(&session, &options.services);
    }

    match session.request_shutdown().map_err(|e| {
        cuenv_core::Error::execution(format!("Failed to request service shutdown: {e}"))
    })? {
        ShutdownRequestOutcome::Signaled { controller_pid } => {
            emit_stdout!(format!(
                "cuenv down: sent shutdown request to controller PID {controller_pid}"
            ));
            Ok("cuenv down: shutdown requested".to_string())
        }
        ShutdownRequestOutcome::Stale { controller_pid } => {
            session.cleanup().map_err(|e| {
                cuenv_core::Error::execution(format!("Failed to clean stale session: {e}"))
            })?;
            emit_stdout!(format!(
                "cuenv down: removed stale session for exited controller PID {controller_pid}"
            ));
            Ok("cuenv down: stale session removed".to_string())
        }
    }
}

fn request_named_service_shutdown(
    session: &SessionManager,
    services: &[String],
) -> cuenv_core::Result<String> {
    if !session.is_alive() {
        return Err(cuenv_core::Error::execution(
            "No active cuenv up session is running. Start services with `cuenv up` first.",
        ));
    }

    for name in services {
        session.read_service(name).map_err(|e| {
            cuenv_core::Error::execution(format!("Service '{name}' not found: {e}"))
        })?;
    }

    for name in services {
        session.request_service_stop(name).map_err(|e| {
            cuenv_core::Error::execution(format!("Failed to queue stop for service '{name}': {e}"))
        })?;
        emit_stdout!(format!("Stop requested for service '{name}'."));
    }

    Ok("cuenv down: service stop requested".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_services::lifecycle::ServiceLifecycle;
    use cuenv_services::session::{ServiceState, SessionInfo};
    use std::fs;

    #[test]
    fn test_down_options_default() {
        let options = DownOptions {
            path: ".".to_string(),
            package: "cuenv".to_string(),
            services: vec![],
        };
        assert_eq!(options.path, ".");
        assert_eq!(options.package, "cuenv");
        assert!(options.services.is_empty());
    }

    #[test]
    fn test_execute_down_queues_named_service_stop() {
        let temp_dir = tempfile::tempdir().unwrap();
        let session = SessionManager::create(temp_dir.path(), "test-project").unwrap();
        seed_service(&session, "db");

        let options = DownOptions {
            path: temp_dir.path().to_string_lossy().into_owned(),
            package: "cuenv".to_string(),
            services: vec!["db".to_string()],
        };
        let result = execute_down(&options).unwrap();
        assert_eq!(result, "cuenv down: service stop requested");
        assert!(session.take_service_stop_request("db").unwrap());
    }

    #[test]
    fn test_execute_down_validates_named_services_before_queueing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let session = SessionManager::create(temp_dir.path(), "test-project").unwrap();
        seed_service(&session, "db");

        let options = DownOptions {
            path: temp_dir.path().to_string_lossy().into_owned(),
            package: "cuenv".to_string(),
            services: vec!["db".to_string(), "missing".to_string()],
        };
        let error = execute_down(&options).unwrap_err().to_string();
        assert!(error.contains("Service 'missing' not found"));
        assert!(!session.take_service_stop_request("db").unwrap());
    }

    #[test]
    fn test_execute_down_cleans_stale_session() {
        let temp_dir = tempfile::tempdir().unwrap();
        seed_stale_session(temp_dir.path());

        let options = DownOptions {
            path: temp_dir.path().to_string_lossy().into_owned(),
            package: "cuenv".to_string(),
            services: Vec::new(),
        };
        let result = execute_down(&options).unwrap();
        assert_eq!(result, "cuenv down: stale session removed");
        assert!(SessionManager::load(temp_dir.path()).is_err());
    }

    fn seed_stale_session(project_path: &Path) {
        let session = SessionManager::create(project_path, "test-project").unwrap();
        let info = SessionInfo {
            version: 1,
            started_at: chrono::Utc::now(),
            controller_pid: 999_999,
            project_path: project_path.to_string_lossy().into_owned(),
            project_name: "test-project".to_string(),
        };
        let json = serde_json::to_string_pretty(&info).unwrap();
        fs::write(session.root().join("session.json"), json).unwrap();
        seed_service(&session, "db");
    }

    fn seed_service(session: &SessionManager, name: &str) {
        session
            .update_service(&ServiceState {
                name: name.to_string(),
                lifecycle: ServiceLifecycle::Ready,
                pid: Some(999_998),
                started_at: Some(chrono::Utc::now()),
                ready_at: Some(chrono::Utc::now()),
                restarts: 0,
                exit_code: None,
                error: None,
                ports: vec![],
            })
            .unwrap();
    }
}
