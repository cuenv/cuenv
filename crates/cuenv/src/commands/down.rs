//! Stub implementation of the `cuenv down` command.
//!
//! This module provides the initial skeleton for tearing down services.
//! In v1, this emits lifecycle events without actually stopping processes.

use cuenv_events::{emit_service_stopping, emit_service_stopped, emit_stdout};

/// Options for the `cuenv down` command.
pub struct DownOptions<'a> {
    /// Path to directory containing CUE files.
    pub path: &'a str,
    /// CUE package name to evaluate.
    pub package: &'a str,
    /// Optional list of service names to bring down (empty = all).
    pub services: &'a [String],
}

/// Execute the stub `cuenv down` command.
///
/// This is a placeholder that prints the service lifecycle events that *would*
/// be emitted by the real orchestrator when tearing down services.
///
/// # Errors
///
/// Returns an error if CUE evaluation or session state reading fails.
pub fn execute_down(options: &DownOptions<'_>) -> cuenv_core::Result<String> {
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
            path: ".",
            package: "cuenv",
            services: &[],
        };
        assert_eq!(options.path, ".");
        assert_eq!(options.package, "cuenv");
        assert!(options.services.is_empty());
    }

    #[test]
    fn test_execute_down_stub_no_services() {
        let options = DownOptions {
            path: ".",
            package: "cuenv",
            services: &[],
        };
        let result = execute_down(&options);
        assert!(result.is_ok());
        assert!(result
            .as_ref()
            .is_ok_and(|s| s.contains("stub complete")));
    }

    #[test]
    fn test_execute_down_stub_with_services() {
        let services = vec!["db".to_string(), "api".to_string()];
        let options = DownOptions {
            path: ".",
            package: "cuenv",
            services: &services,
        };
        let result = execute_down(&options);
        assert!(result.is_ok());
    }
}
