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

        let dependency_plan =
            DependencyPlanner::new(&project.tasks, &project.images, &project.services)
                .plan(&filtered_services)?;

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
        ServiceTaskContext {
            tasks: &tasks,
            project_env: project_env.as_ref(),
            environment: options.environment.as_deref(),
            project_root: &target_path,
            cue_module_root,
        },
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

/// Walks selected services' `dependsOn` edges to classify each dependency as a
/// task root (executed before startup) or an image root, recursing into image
/// task dependencies. The task index is built once and reused for existence
/// probes instead of re-cloning per dependency.
struct DependencyPlanner<'a> {
    tasks: &'a HashMap<String, TaskNode>,
    images: &'a HashMap<String, ContainerImage>,
    all_services: &'a HashMap<String, Service>,
    task_index: Tasks,
    seen_tasks: HashSet<String>,
    seen_images: HashSet<String>,
    plan: ServiceDependencyPlan,
    unknown: Vec<String>,
}

impl<'a> DependencyPlanner<'a> {
    fn new(
        tasks: &'a HashMap<String, TaskNode>,
        images: &'a HashMap<String, ContainerImage>,
        all_services: &'a HashMap<String, Service>,
    ) -> Self {
        Self {
            tasks,
            images,
            all_services,
            task_index: Tasks {
                tasks: tasks.clone(),
            },
            seen_tasks: HashSet::new(),
            seen_images: HashSet::new(),
            plan: ServiceDependencyPlan::default(),
            unknown: Vec::new(),
        }
    }

    fn plan(
        mut self,
        services: &HashMap<String, Service>,
    ) -> cuenv_core::Result<ServiceDependencyPlan> {
        for (service_name, service) in services {
            for dependency in &service.depends_on {
                let name = dependency.task_name();
                if self.all_services.contains_key(name) {
                    continue;
                }

                if self.task_exists(name) {
                    self.push_task(name);
                } else if self.images.contains_key(name) {
                    self.push_image_root(name);
                } else {
                    self.unknown
                        .push(format!("{service_name} depends on '{name}'"));
                }
            }
        }

        if self.unknown.is_empty() {
            Ok(self.plan)
        } else {
            Err(cuenv_core::Error::configuration(format!(
                "cuenv up service dependencies were not found: {}",
                self.unknown.join("; ")
            )))
        }
    }

    fn push_task(&mut self, name: &str) {
        if self.seen_tasks.insert(name.to_string()) {
            self.plan.task_roots.push(name.to_string());
        }
    }

    fn push_image_root(&mut self, name: &str) {
        if self.seen_images.insert(name.to_string()) {
            self.plan.image_roots.push(name.to_string());
        }
        self.collect_image_task_dependencies(name);
    }

    fn collect_image_task_dependencies(&mut self, image_name: &str) {
        // Copy dependency names out first so the immutable borrow of `images`
        // is released before the recursive `&mut self` calls below.
        let Some(image) = self.images.get(image_name) else {
            return;
        };
        let dependencies: Vec<String> = image
            .depends_on
            .iter()
            .map(|dependency| dependency.task_name().to_string())
            .collect();

        for name in dependencies {
            if self.task_exists(&name) {
                self.push_task(&name);
            } else if self.images.contains_key(&name) && self.seen_images.insert(name.clone()) {
                self.collect_image_task_dependencies(&name);
            }
        }
    }

    fn task_exists(&self, name: &str) -> bool {
        if self.tasks.contains_key(name) {
            return true;
        }
        let mut graph = TaskGraph::new();
        graph
            .build_for_task(name, &self.task_index)
            .is_ok_and(|()| graph.task_count() > 0)
    }
}

/// Shared context for executing a service's resolved dependency roots.
struct ServiceTaskContext<'a> {
    tasks: &'a HashMap<String, TaskNode>,
    project_env: Option<&'a Env>,
    environment: Option<&'a str>,
    project_root: &'a Path,
    cue_module_root: Option<PathBuf>,
}

async fn execute_service_dependencies(
    plan: &ServiceDependencyPlan,
    context: ServiceTaskContext<'_>,
) -> cuenv_core::Result<()> {
    // Image roots are resolved by the DependencyPlanner, which already walks
    // their `dependsOn` edges and collects the transitive task deps into
    // `task_roots`. Running those task roots builds the images via the task
    // DAG, making them available in the local Docker daemon before services
    // start. Log an informational message when image builds are involved.
    if !plan.image_roots.is_empty() {
        emit_stdout!(format!(
            "cuenv up: image dependencies will be built via their task roots: {}",
            plan.image_roots.join(", ")
        ));
    }

    if !plan.task_roots.is_empty() {
        execute_service_task_dependencies(&plan.task_roots, context).await?;
    }

    Ok(())
}

async fn execute_service_task_dependencies(
    task_roots: &[String],
    context: ServiceTaskContext<'_>,
) -> cuenv_core::Result<()> {
    let all_tasks = Tasks {
        tasks: context.tasks.clone(),
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
        environment: task_dependency_environment(context.project_env, context.environment).await?,
        working_dir: None,
        project_root: context.project_root.to_path_buf(),
        cue_module_root: context.cue_module_root,
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
            installable: None,
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
            DependencyPlanner::new(&HashMap::new(), &HashMap::new(), &services).plan(&services)?;

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

        let result = DependencyPlanner::new(&tasks, &HashMap::new(), &services).plan(&services)?;

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

        let result = DependencyPlanner::new(&tasks, &images, &services).plan(&services)?;

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
