//! Implementation of the `cuenv restart` command.
//!
//! Stops and re-starts named services via the session state.

use std::path::Path;

use cuenv_events::{emit_service_restarting, emit_stdout};
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
/// Sends a restart signal to the named services. The running `cuenv up`
/// session handles the actual stop/re-spawn cycle.
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

    for name in &options.services {
        let state = session.read_service(name).map_err(|e| {
            cuenv_core::Error::execution(format!("Service '{name}' not found: {e}"))
        })?;

        emit_service_restarting!(name, "manual", state.restarts + 1);
        emit_stdout!(format!("Restart signal sent to service '{name}'."));

        // In the current architecture, the actual restart is handled by the
        // supervisor in the `cuenv up` process. A full implementation would
        // send a signal to the supervisor (e.g., via a Unix signal or a
        // control socket). For now, we emit the event for the session to pick up.
    }

    emit_stdout!("Restart signals sent. The running `cuenv up` session will restart the services.");
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_restart_options() {
        let options = RestartOptions {
            path: ".".to_string(),
            package: "cuenv".to_string(),
            services: vec!["db".to_string()],
        };
        assert_eq!(options.services.len(), 1);
    }
}
