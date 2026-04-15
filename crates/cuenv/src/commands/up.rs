//! Implementation of the `cuenv up` command.
//!
//! Evaluates the CUE configuration, discovers services, builds a mixed
//! task/service dependency graph, and delegates to the `ServiceController`
//! for process supervision.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use cuenv_core::environment::EnvValue;
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
    /// Optional environment name to apply (e.g., "test", "production").
    pub environment: Option<String>,
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
    // Become a subreaper so that any descendants orphaned by a service
    // crash or double-fork are re-parented to cuenv and can be reaped
    // here instead of drifting to pid 1.
    install_process_supervisor();

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

        let mut filtered_services =
            filter_services(&project.services, &options.services, &options.labels);

        if filtered_services.is_empty() {
            emit_stdout!("cuenv up: no services match the specified filters");
            return Ok(());
        }

        // Propagate the project-level environment (selected by `-e` if set)
        // into each service's env map so secrets, interpolation, and policy
        // filtering all apply in `supervisor::spawn_process`. Per-service
        // env entries take precedence over the project env.
        if let Some(env) = &project.env {
            let project_env_vars = match options.environment.as_deref() {
                Some(name) => env.for_environment(name),
                None => env.base.clone(),
            };
            if !project_env_vars.is_empty() {
                for service in filtered_services.values_mut() {
                    merge_project_env_into_service(&project_env_vars, &mut service.env);
                }
            }
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

        let graph = build_mixed_graph(&project.tasks, &filtered_services, &project.images)
            .map_err(|e| {
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

/// Configure cuenv as a process supervisor for its service subtree.
///
/// On Linux, promotes the process to a subreaper (so orphaned descendants
/// re-parent here instead of pid 1) and spawns a background tokio task
/// that periodically reaps zombies with `waitpid(-1, WNOHANG)`.
///
/// On other platforms this is a no-op; the per-service `__supervise`
/// wrapper handles reaping for its own subtree on macOS.
fn install_process_supervisor() {
    #[cfg(target_os = "linux")]
    {
        #[expect(
            unsafe_code,
            reason = "PR_SET_CHILD_SUBREAPER affects only the calling process"
        )]
        // SAFETY: PR_SET_CHILD_SUBREAPER affects only the calling
        // process. A non-zero value enables the behaviour.
        unsafe {
            libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1);
        }

        tokio::spawn(async {
            loop {
                #[expect(
                    unsafe_code,
                    reason = "waitpid(-1, _, WNOHANG) is non-blocking and only reaps exited descendants"
                )]
                // SAFETY: waitpid(-1, _, WNOHANG) is always safe; it
                // never blocks and only collects already-exited
                // descendants. The returned status is discarded.
                unsafe {
                    while libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) > 0 {}
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        });
    }
}

/// Merge project-level environment variables into a service's env map.
/// Service-level entries win over project-level entries.
fn merge_project_env_into_service(
    project_env: &HashMap<String, EnvValue>,
    service_env: &mut HashMap<String, EnvValue>,
) {
    for (key, value) in project_env {
        service_env
            .entry(key.clone())
            .or_insert_with(|| value.clone());
    }
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
            environment: None,
        };
        assert_eq!(options.path, ".");
        assert_eq!(options.package, "cuenv");
        assert!(options.services.is_empty());
        assert!(options.labels.is_empty());
        assert!(options.environment.is_none());
    }

    #[test]
    fn test_merge_project_env_into_service_preserves_service_values() {
        let mut project_env = HashMap::new();
        project_env.insert("A".to_string(), EnvValue::String("project-a".to_string()));
        project_env.insert("B".to_string(), EnvValue::String("project-b".to_string()));

        let mut service_env = HashMap::new();
        service_env.insert("B".to_string(), EnvValue::String("service-b".to_string()));

        merge_project_env_into_service(&project_env, &mut service_env);

        assert_eq!(
            service_env.get("A"),
            Some(&EnvValue::String("project-a".to_string()))
        );
        // Service value wins over project value.
        assert_eq!(
            service_env.get("B"),
            Some(&EnvValue::String("service-b".to_string()))
        );
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
