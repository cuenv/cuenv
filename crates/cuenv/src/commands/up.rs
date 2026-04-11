//! Implementation of the `cuenv up` command.
//!
//! Evaluates the CUE configuration, discovers services, builds a mixed
//! task/service dependency graph, and delegates to the `ServiceController`
//! for process supervision.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use cuenv_core::manifest::{Project, Service};
use cuenv_events::emit_stdout;
use cuenv_services::controller::{ControllerConfig, ServiceController, build_mixed_graph};
use cuenv_services::session::SessionManager;

use super::{CommandExecutor, relative_path_from_root};

/// Options for the `cuenv up` command.
pub struct UpOptions {
    /// Path to directory containing CUE files.
    pub path: String,
    /// CUE package name to evaluate.
    pub package: String,
    /// Optional list of service names to bring up (empty = all).
    pub services: Vec<String>,
    /// Optional label filters.
    pub labels: Vec<String>,
}

/// Execute the `cuenv up` command.
///
/// Evaluates CUE configuration, discovers services, builds a mixed dependency
/// graph, and runs the service controller for process supervision.
///
/// # Errors
///
/// Returns an error if CUE evaluation, graph construction, or service
/// supervision fails.
pub async fn execute_up(options: &UpOptions, executor: &CommandExecutor) -> cuenv_core::Result<()> {
    let target_path =
        Path::new(&options.path)
            .canonicalize()
            .map_err(|e| cuenv_core::Error::Io {
                source: e,
                path: Some(Path::new(&options.path).to_path_buf().into_boxed_path()),
                operation: "canonicalize path".to_string(),
            })?;

    emit_stdout!(format!(
        "cuenv up: evaluating services in {} (package: {})",
        options.path, options.package
    ));

    // Evaluate CUE and deserialize project.
    //
    // The module guard holds a MutexGuard (not Send), so we must extract
    // everything we need and drop it before any .await point.
    let (filtered_services, graph, session) = {
        let module = executor.get_module(&target_path)?;
        let relative_path = relative_path_from_root(&module.root, &target_path);

        let instance = module.get(&relative_path).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "No CUE instance found at path: {} (relative: {})",
                target_path.display(),
                relative_path.display()
            ))
        })?;

        let project: Project = instance.deserialize()?;

        if project.services.is_empty() {
            emit_stdout!("cuenv up: no services defined in configuration");
            return Ok(());
        }

        let filtered_services =
            filter_services(&project.services, &options.services, &options.labels);

        if filtered_services.is_empty() {
            emit_stdout!("cuenv up: no services match the specified filters");
            return Ok(());
        }

        emit_stdout!(format!(
            "cuenv up: starting {} service(s): {}",
            filtered_services.len(),
            filtered_services
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));

        let graph = build_mixed_graph(&project.tasks, &filtered_services).map_err(|e| {
            cuenv_core::Error::execution(format!("Failed to build service graph: {e}"))
        })?;

        let session = SessionManager::create(&target_path, &project.name)
            .map_err(|e| cuenv_core::Error::execution(format!("Failed to create session: {e}")))?;

        (filtered_services, graph, session)
    };
    // ModuleGuard is now dropped — safe to .await below

    // Set up shutdown signal
    let shutdown = CancellationToken::new();
    let shutdown_signal = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        shutdown_signal.cancel();
    });

    // Run the service controller
    let controller = ServiceController::new(
        ControllerConfig {
            project_root: target_path,
        },
        shutdown,
    );

    controller
        .execute_up(&graph, &filtered_services, Arc::new(session))
        .await
        .map_err(|e| cuenv_core::Error::execution(format!("Service controller failed: {e}")))
}

/// Filter services by name and label.
fn filter_services(
    all_services: &HashMap<String, Service>,
    names: &[String],
    labels: &[String],
) -> HashMap<String, Service> {
    all_services
        .iter()
        .filter(|(name, service)| {
            let name_match = names.is_empty() || names.contains(name);
            let label_match =
                labels.is_empty() || labels.iter().any(|l| service.labels.contains(l));
            name_match && label_match
        })
        .map(|(name, service)| (name.clone(), service.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_up_options_default() {
        let options = UpOptions {
            path: ".".to_string(),
            package: "cuenv".to_string(),
            services: vec![],
            labels: vec![],
        };
        assert_eq!(options.path, ".");
        assert_eq!(options.package, "cuenv");
        assert!(options.services.is_empty());
        assert!(options.labels.is_empty());
    }

    #[test]
    fn test_filter_services_no_filters() {
        let mut services = HashMap::new();
        services.insert("db".to_string(), Service::default());
        services.insert("api".to_string(), Service::default());

        let result = filter_services(&services, &[], &[]);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_filter_services_by_name() {
        let mut services = HashMap::new();
        services.insert("db".to_string(), Service::default());
        services.insert("api".to_string(), Service::default());

        let result = filter_services(&services, &["db".to_string()], &[]);
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("db"));
    }
}
