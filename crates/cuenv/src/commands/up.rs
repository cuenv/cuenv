//! Implementation of the `cuenv up` command.
//!
//! Evaluates the CUE configuration, discovers services, builds a mixed
//! task/service dependency graph, and delegates to the `ServiceController`
//! for process supervision.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use cuenv_core::OutputCapture;
use cuenv_core::environment::{Env, EnvValue, Environment};
use cuenv_core::manifest::{ContainerImage, Project, Service};
use cuenv_core::tasks::{ExecutorConfig, TaskExecutor, TaskGraph, TaskNode, Tasks};
use cuenv_events::emit_stdout;
use cuenv_services::controller::{ControllerConfig, ServiceController, build_service_graph};
use cuenv_services::session::SessionManager;

use super::env_file::find_cue_module_root;
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
    let (filtered_services, graph, session, dependency_plan, tasks, project_env, cue_module_root) = {
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

        include_service_dependencies(&mut filtered_services, &project.services);

        // Propagate the project-level environment (selected by `-e` if set)
        // into each service's env map so secrets, interpolation, and policy
        // filtering all apply in `process::ServiceProcess::spawn`. Per-service
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

        let dependency_plan = service_dependency_plan(
            &filtered_services,
            &project.tasks,
            &project.images,
            &project.services,
        )?;

        let graph = build_service_graph(&filtered_services).map_err(|e| {
            cuenv_core::Error::execution(format!("Failed to build service graph: {e}"))
        })?;

        let session = SessionManager::create(&target_path, &project.name)
            .map_err(|e| cuenv_core::Error::execution(format!("Failed to create session: {e}")))?;

        (
            filtered_services,
            graph,
            session,
            dependency_plan,
            project.tasks,
            project.env,
            find_cue_module_root(&target_path),
        )
    };
    // ModuleGuard is now dropped — safe to .await below

    execute_service_dependencies(
        &dependency_plan,
        &tasks,
        project_env.as_ref(),
        options.environment.as_deref(),
        &target_path,
        cue_module_root,
    )
    .await?;

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
/// On Linux, promotes the process to a subreaper so that orphaned
/// descendants re-parent to cuenv instead of drifting to pid 1. Direct
/// children of cuenv (the supervised services) are reaped via
/// `tokio::process::Child::wait`; we intentionally do not run a
/// `waitpid(-1, _, WNOHANG)` loop here because it would race with and
/// steal exit statuses from the per-service waiters. Orphans that
/// re-parent to cuenv will accumulate as zombies until process exit,
/// which is acceptable for the lifetime of `cuenv up`.
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

#[derive(Debug, Default)]
struct ServiceDependencyPlan {
    task_roots: Vec<String>,
    image_roots: Vec<String>,
}

fn service_dependency_plan(
    services: &HashMap<String, Service>,
    tasks: &HashMap<String, TaskNode>,
    images: &HashMap<String, ContainerImage>,
    all_services: &HashMap<String, Service>,
) -> cuenv_core::Result<ServiceDependencyPlan> {
    let mut plan = ServiceDependencyPlan::default();
    let mut seen_tasks = HashSet::new();
    let mut seen_images = HashSet::new();
    let mut unknown = Vec::new();

    for (service_name, service) in services {
        for dependency in &service.depends_on {
            let dependency_name = dependency.task_name();
            if all_services.contains_key(dependency_name) {
                continue;
            }

            if task_dependency_exists(dependency_name, tasks) {
                if seen_tasks.insert(dependency_name.to_string()) {
                    plan.task_roots.push(dependency_name.to_string());
                }
            } else if images.contains_key(dependency_name) {
                if seen_images.insert(dependency_name.to_string()) {
                    plan.image_roots.push(dependency_name.to_string());
                }
                collect_image_task_dependencies(
                    dependency_name,
                    images,
                    tasks,
                    &mut seen_images,
                    &mut seen_tasks,
                    &mut plan.task_roots,
                );
            } else {
                unknown.push(format!("{service_name} depends on '{dependency_name}'"));
            }
        }
    }

    if unknown.is_empty() {
        Ok(plan)
    } else {
        Err(cuenv_core::Error::configuration(format!(
            "cuenv up service dependencies were not found: {}",
            unknown.join("; ")
        )))
    }
}

fn collect_image_task_dependencies(
    image_name: &str,
    images: &HashMap<String, ContainerImage>,
    tasks: &HashMap<String, TaskNode>,
    seen_images: &mut HashSet<String>,
    seen_tasks: &mut HashSet<String>,
    task_roots: &mut Vec<String>,
) {
    let Some(image) = images.get(image_name) else {
        return;
    };

    for dependency in &image.depends_on {
        let dependency_name = dependency.task_name();
        if task_dependency_exists(dependency_name, tasks) {
            if seen_tasks.insert(dependency_name.to_string()) {
                task_roots.push(dependency_name.to_string());
            }
        } else if images.contains_key(dependency_name)
            && seen_images.insert(dependency_name.to_string())
        {
            collect_image_task_dependencies(
                dependency_name,
                images,
                tasks,
                seen_images,
                seen_tasks,
                task_roots,
            );
        }
    }
}

fn task_dependency_exists(name: &str, tasks: &HashMap<String, TaskNode>) -> bool {
    if tasks.contains_key(name) {
        return true;
    }

    let all_tasks = Tasks {
        tasks: tasks.clone(),
    };
    let mut graph = TaskGraph::new();
    graph
        .build_for_task(name, &all_tasks)
        .is_ok_and(|()| graph.task_count() > 0)
}

async fn execute_service_dependencies(
    plan: &ServiceDependencyPlan,
    tasks: &HashMap<String, TaskNode>,
    project_env: Option<&Env>,
    environment: Option<&str>,
    project_root: &Path,
    cue_module_root: Option<PathBuf>,
) -> cuenv_core::Result<()> {
    if !plan.task_roots.is_empty() {
        execute_service_task_dependencies(
            &plan.task_roots,
            tasks,
            project_env,
            environment,
            project_root,
            cue_module_root,
        )
        .await?;
    }

    if !plan.image_roots.is_empty() {
        return Err(cuenv_core::Error::configuration(format!(
            "cuenv up resolved image dependencies but image execution backends are not implemented yet: {}",
            plan.image_roots.join(", ")
        )));
    }

    Ok(())
}

async fn execute_service_task_dependencies(
    task_roots: &[String],
    tasks: &HashMap<String, TaskNode>,
    project_env: Option<&Env>,
    environment: Option<&str>,
    project_root: &Path,
    cue_module_root: Option<PathBuf>,
) -> cuenv_core::Result<()> {
    let all_tasks = Tasks {
        tasks: tasks.clone(),
    };
    let mut task_graph = TaskGraph::new();
    for task_name in task_roots {
        task_graph.build_for_task(task_name, &all_tasks)?;
    }

    if task_graph.task_count() == 0 {
        return Ok(());
    }

    emit_stdout!(format!(
        "cuenv up: running service task dependencies: {}",
        task_roots.join(", ")
    ));

    let executor = build_up_task_executor(ExecutorConfig {
        capture_output: OutputCapture::Capture,
        max_parallel: 0,
        continue_on_error: false,
        environment: task_dependency_environment(project_env, environment).await?,
        working_dir: None,
        project_root: project_root.to_path_buf(),
        cue_module_root,
        materialize_outputs: None,
        cache_dir: None,
        show_cache_path: false,
        backend_config: None,
        cli_backend: None,
        cache: None,
    });

    executor.execute_graph(&task_graph).await?;
    Ok(())
}

async fn task_dependency_environment(
    project_env: Option<&Env>,
    environment: Option<&str>,
) -> cuenv_core::Result<Environment> {
    let mut runtime_env = Environment::new();
    let Some(env) = project_env else {
        return Ok(runtime_env);
    };

    let env_vars = environment.map_or_else(|| env.base.clone(), |name| env.for_environment(name));
    let (resolved, secrets) =
        Environment::resolve_for_task_with_secrets("service dependencies", &env_vars).await?;
    cuenv_events::register_secrets(secrets);

    for (key, value) in resolved {
        runtime_env.set(key, value);
    }

    Ok(runtime_env)
}

#[cfg(feature = "dagger-backend")]
fn build_up_task_executor(config: ExecutorConfig) -> TaskExecutor {
    TaskExecutor::with_dagger_factory(config, Some(cuenv_dagger::create_dagger_backend))
}

#[cfg(not(feature = "dagger-backend"))]
fn build_up_task_executor(config: ExecutorConfig) -> TaskExecutor {
    TaskExecutor::new(config)
}

fn include_service_dependencies(
    selected_services: &mut HashMap<String, Service>,
    all_services: &HashMap<String, Service>,
) {
    let mut pending: Vec<String> = selected_services
        .values()
        .flat_map(|service| service.depends_on.iter().map(|dep| dep.name.clone()))
        .collect();

    while let Some(dependency_name) = pending.pop() {
        if selected_services.contains_key(&dependency_name) {
            continue;
        }

        if let Some(service) = all_services.get(&dependency_name) {
            pending.extend(service.depends_on.iter().map(|dep| dep.name.clone()));
            selected_services.insert(dependency_name, service.clone());
        }
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
    use cuenv_core::manifest::ImageOutputRef;
    use cuenv_core::tasks::TaskDependency;

    fn test_image(depends_on: Vec<&str>) -> ContainerImage {
        ContainerImage {
            image_type: "image".to_string(),
            ref_output: ImageOutputRef {
                cuenv_output_ref: true,
                cuenv_image: "test".to_string(),
                cuenv_output: "ref".to_string(),
            },
            digest: ImageOutputRef {
                cuenv_output_ref: true,
                cuenv_image: "test".to_string(),
                cuenv_output: "digest".to_string(),
            },
            context: ".".to_string(),
            dockerfile: "Dockerfile".to_string(),
            build_args: HashMap::new(),
            target: None,
            tags: vec![],
            registry: None,
            repository: None,
            platform: vec![],
            depends_on: depends_on
                .into_iter()
                .map(TaskDependency::from_name)
                .collect(),
            labels: vec![],
            inputs: vec![],
            description: None,
        }
    }

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
    fn service_dependency_plan_allows_service_deps() -> cuenv_core::Result<()> {
        let mut services = HashMap::new();
        services.insert("db".to_string(), Service::default());
        services.insert(
            "api".to_string(),
            Service {
                depends_on: vec![TaskDependency::from_name("db")],
                ..Service::default()
            },
        );

        let result =
            service_dependency_plan(&services, &HashMap::new(), &HashMap::new(), &services)?;

        assert!(result.task_roots.is_empty());
        assert!(result.image_roots.is_empty());
        Ok(())
    }

    #[test]
    fn service_dependency_plan_collects_task_deps() -> cuenv_core::Result<()> {
        let mut services = HashMap::new();
        services.insert(
            "api".to_string(),
            Service {
                depends_on: vec![TaskDependency::from_name("build")],
                ..Service::default()
            },
        );

        let mut tasks = HashMap::new();
        tasks.insert("build".to_string(), TaskNode::Task(Box::default()));

        let result = service_dependency_plan(&services, &tasks, &HashMap::new(), &services)?;

        assert_eq!(result.task_roots, vec!["build"]);
        assert!(result.image_roots.is_empty());
        Ok(())
    }

    #[test]
    fn service_dependency_plan_collects_image_deps() -> cuenv_core::Result<()> {
        let mut services = HashMap::new();
        services.insert(
            "api".to_string(),
            Service {
                depends_on: vec![TaskDependency::from_name("api-image")],
                ..Service::default()
            },
        );

        let mut tasks = HashMap::new();
        tasks.insert("build".to_string(), TaskNode::Task(Box::default()));

        let mut images = HashMap::new();
        images.insert("api-image".to_string(), test_image(vec!["build"]));

        let result = service_dependency_plan(&services, &tasks, &images, &services)?;

        assert_eq!(result.task_roots, vec!["build"]);
        assert_eq!(result.image_roots, vec!["api-image"]);
        Ok(())
    }

    #[test]
    fn include_service_dependencies_adds_transitive_deps() {
        let mut all_services = HashMap::new();
        all_services.insert("db".to_string(), Service::default());
        all_services.insert(
            "api".to_string(),
            Service {
                depends_on: vec![TaskDependency::from_name("db")],
                ..Service::default()
            },
        );

        let mut selected = filter_services(&all_services, &["api".to_string()], &[]);
        include_service_dependencies(&mut selected, &all_services);

        assert!(selected.contains_key("api"));
        assert!(selected.contains_key("db"));
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
