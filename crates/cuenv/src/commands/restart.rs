//! Implementation of the `cuenv restart` command.
//!
//! Queues restart requests for named services via the persisted session state.

use std::path::Path;

use cuenv_events::emit_stdout;
use cuenv_services::session::SessionManager;

/// Options for the `cuenv restart` command.
pub struct RestartOptions {
    /// Path to directory containing CUE files.
    pub path: String,
    /// CUE package name to evaluate.
    pub package: String,
    /// Service names to restart.
    pub services: Vec<String>,
}

/// Execute the `cuenv restart` command.
///
/// Queues restart requests for named services. The running `cuenv up`
/// supervisors consume those requests and perform the stop/re-spawn cycle.
///
/// # Errors
///
/// Returns an error if no session exists or the services can't be found.
pub fn execute_restart(options: &RestartOptions) -> cuenv_core::Result<String> {
    let project_path = Path::new(&options.path);
    let session = SessionManager::load(project_path)
        .map_err(|e| cuenv_core::Error::execution(format!("Failed to load session: {e}")))?;

    if !session.is_alive() {
        return Err(cuenv_core::Error::execution(
            "No active cuenv up session is running. Start services with `cuenv up` first.",
        ));
    }

    emit_stdout!(format!(
        "cuenv restart: queuing restart requests in {} (package: {})",
        options.path, options.package
    ));

    for name in &options.services {
        session.read_service(name).map_err(|e| {
            cuenv_core::Error::execution(format!("Service '{name}' not found: {e}"))
        })?;
    }

    for name in &options.services {
        session.request_service_restart(name).map_err(|e| {
            cuenv_core::Error::execution(format!(
                "Failed to queue restart for service '{name}': {e}"
            ))
        })?;
        emit_stdout!(format!("Restart requested for service '{name}'."));
    }

    emit_stdout!(
        "Restart requests queued. The active `cuenv up` session will restart the services."
    );
    Ok("cuenv restart: restart requested".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_services::lifecycle::ServiceLifecycle;
    use cuenv_services::session::ServiceState;

    #[test]
    fn test_restart_options() {
        let options = RestartOptions {
            path: ".".to_string(),
            package: "cuenv".to_string(),
            services: vec!["db".to_string()],
        };
        assert_eq!(options.services.len(), 1);
    }

    #[test]
    fn test_execute_restart_queues_request() {
        let temp_dir = tempfile::tempdir().unwrap();
        let session = SessionManager::create(temp_dir.path(), "test-project").unwrap();
        seed_service(&session, "db");

        let options = RestartOptions {
            path: temp_dir.path().to_string_lossy().into_owned(),
            package: "cuenv".to_string(),
            services: vec!["db".to_string()],
        };

        let result = execute_restart(&options).unwrap();
        assert_eq!(result, "cuenv restart: restart requested");
        assert!(session.take_service_restart_request("db").unwrap());
    }

    #[test]
    fn test_execute_restart_rejects_missing_service() {
        let temp_dir = tempfile::tempdir().unwrap();
        let _session = SessionManager::create(temp_dir.path(), "test-project").unwrap();

        let options = RestartOptions {
            path: temp_dir.path().to_string_lossy().into_owned(),
            package: "cuenv".to_string(),
            services: vec!["missing".to_string()],
        };

        let error = execute_restart(&options).unwrap_err().to_string();
        assert!(error.contains("Service 'missing' not found"));
    }

    #[test]
    fn test_execute_restart_validates_all_services_before_queueing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let session = SessionManager::create(temp_dir.path(), "test-project").unwrap();
        seed_service(&session, "db");

        let options = RestartOptions {
            path: temp_dir.path().to_string_lossy().into_owned(),
            package: "cuenv".to_string(),
            services: vec!["db".to_string(), "missing".to_string()],
        };

        let error = execute_restart(&options).unwrap_err().to_string();
        assert!(error.contains("Service 'missing' not found"));
        assert!(!session.take_service_restart_request("db").unwrap());
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
            })
            .unwrap();
    }
}
