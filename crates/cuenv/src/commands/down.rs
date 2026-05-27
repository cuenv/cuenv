//! Implementation of the `cuenv down` command.
//!
//! The command operates on the persisted service session created by
//! `cuenv up` and requests graceful shutdown of that session's controller.

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
/// controller. Per-service shutdown requires supervisor IPC and is rejected
/// rather than pretending to stop services that the controller may restart.
///
/// # Errors
///
/// Returns an error if no session exists, session state cannot be read, a
/// controller cannot be signalled, or per-service shutdown is requested.
pub fn execute_down(options: &DownOptions) -> cuenv_core::Result<String> {
    reject_partial_shutdown(&options.services)?;

    let session = SessionManager::load(Path::new(&options.path))
        .map_err(|e| cuenv_core::Error::execution(format!("Failed to load session: {e}")))?;

    emit_stdout!(format!(
        "cuenv down: requesting shutdown for services in {} (package: {})",
        options.path, options.package
    ));

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

fn reject_partial_shutdown(services: &[String]) -> cuenv_core::Result<()> {
    if services.is_empty() {
        return Ok(());
    }

    Err(cuenv_core::Error::execution(
        "cuenv down currently stops the whole active service session only; \
         omit service names to stop all services",
    ))
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
    fn test_execute_down_rejects_named_services() {
        let options = DownOptions {
            path: ".".to_string(),
            package: "cuenv".to_string(),
            services: vec!["db".to_string()],
        };
        let error = execute_down(&options).unwrap_err().to_string();
        assert!(error.contains("whole active service session only"));
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
        session
            .update_service(&ServiceState {
                name: "db".to_string(),
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
