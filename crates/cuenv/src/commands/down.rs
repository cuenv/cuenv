//! Stub implementation of the `cuenv down` command.
//!
//! This module provides the initial skeleton for tearing down services.
//! The real implementation will delegate to `cuenv_services::SessionManager`
//! for process teardown.

use cuenv_events::{emit_service_stopping, emit_service_stopped, emit_stdout};

/// Options for the `cuenv down` command.
pub struct DownOptions {
    /// Path to directory containing CUE files.
    pub path: String,
    /// CUE package name to evaluate.
    pub package: String,
    /// Optional list of service names to bring down (empty = all).
    pub services: Vec<String>,
}

/// Execute the stub `cuenv down` command.
///
/// This is a placeholder that prints the service lifecycle events that *would*
/// be emitted by the real orchestrator when tearing down services.
///
/// # Errors
///
/// Returns an error if CUE evaluation or session state reading fails.
pub fn execute_down(options: &DownOptions) -> cuenv_core::Result<String> {
    emit_stdout!(format!(
        "cuenv down: tearing down services in {} (package: {})",
        options.path, options.package
    ));

    if options.services.is_empty() {
        emit_stdout!("cuenv down: no service names specified — would tear down all services");
    } else {
        // Reverse order to respect dependency ordering
        for svc in options.services.iter().rev() {
            emit_service_stopping!(svc);
            emit_service_stopped!(svc, Some(0));
        }
    }

    Ok("cuenv down: stub complete".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_execute_down_stub_no_services() {
        let options = DownOptions {
            path: ".".to_string(),
            package: "cuenv".to_string(),
            services: vec![],
        };
        let result = execute_down(&options);
        assert!(result.is_ok());
        assert!(result
            .as_ref()
            .is_ok_and(|s| s.contains("stub complete")));
    }

    #[test]
    fn test_execute_down_stub_with_services() {
        let options = DownOptions {
            path: ".".to_string(),
            package: "cuenv".to_string(),
            services: vec!["db".to_string(), "api".to_string()],
        };
        let result = execute_down(&options);
        assert!(result.is_ok());
    }
}
