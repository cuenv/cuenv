//! Implementation of the `cuenv up` command.
//!
//! Evaluates the CUE configuration, discovers services, builds a mixed
//! task/service dependency graph, and delegates to the `ServiceController`
//! for process supervision.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

use cuenv_core::OutputCapture;
use cuenv_core::environment::{Env, EnvValue, Environment};
use cuenv_core::manifest::{ContainerImage, Project, Service};
use cuenv_core::tasks::output_refs::OutputRefDep;
use cuenv_core::tasks::{Task, TaskNode, Tasks};
use cuenv_events::emit_stdout;
use cuenv_services::controller::{ControllerConfig, ServiceController};
use cuenv_services::session::SessionManager;
use cuenv_task_exec::{
    ExecutorConfig, ExternalTaskName, ProjectExecution, RootProject, ScopeProject,
    TaskEnvironmentFuture, TaskEnvironments, TaskExecutor, TaskGraph, TaskScope,
};

use super::task::projects::{self, ModuleProjects, WorkspaceContributors};
use super::{CommandExecutor, env_file};

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

    let target_path = Path::new(&options.path).canonicalize().map_err(|e| {
        cuenv_core::Error::io_with_path(
            "canonicalize path",
            Path::new(&options.path).to_path_buf(),
            e,
        )
    })?;

    emit_stdout!(format!(
        "cuenv up: evaluating services in {} (package: {})",
        options.path, options.package
    ));

    // Evaluate CUE and deserialize project.
    //
    // The module guard holds a MutexGuard (not Send), so we must extract
    // everything we need and drop it before any .await point.
    let (filtered_services, graph, session, dependency_plan, project, output_ref_deps) = {
        let module = executor.module(&target_path)?;
        let relative_path = super::relative_path_from_root(&module.root, &target_path);

        let instance = module.get(&relative_path).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "No CUE instance found at path: {} (relative: {})",
                target_path.display(),
                relative_path.display()
            ))
        })?;

        // Inputs are expanded here as they are for `cuenv task`: a task
        // that consumes another task's output depends on it, in this
        // project or another.
        let project: Project = instance.to_project()?;
        let output_ref_deps = instance.output_ref_deps.clone();

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
        reject_reached_problems(&project, &filtered_services, &dependency_plan)?;

        let graph =
            cuenv_services::controller::build_service_graph(&filtered_services).map_err(|e| {
                cuenv_core::Error::execution(format!("Failed to build service graph: {e}"))
            })?;

        let session = SessionManager::create(&target_path, &project.name)
            .map_err(|e| cuenv_core::Error::execution(format!("Failed to create session: {e}")))?;

        (
            filtered_services,
            graph,
            session,
            dependency_plan,
            project,
            output_ref_deps,
        )
    };
    // ModuleGuard is now dropped — safe to .await below

    execute_service_dependencies(
        &dependency_plan,
        ServiceTaskContext {
            project,
            output_ref_deps,
            environment: options.environment.as_deref(),
            project_root: &target_path,
            cue_module_root: env_file::find_cue_module_root(&target_path),
            executor,
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
/// Fail when a service about to start, or an image it depends on, has a
/// problem recorded when the project's value was read: a dependency on
/// another project's task or image, which is not supported yet. The tasks
/// they depend on are checked when their graph is built.
fn reject_reached_problems(
    project: &Project,
    services: &HashMap<String, Service>,
    plan: &ServiceDependencyPlan,
) -> cuenv_core::Result<()> {
    let mut problems: Vec<&String> = services
        .keys()
        .flat_map(|name| project.diagnostics.service(name))
        .collect();
    problems.extend(
        plan.image_roots
            .iter()
            .flat_map(|name| project.diagnostics.image(name)),
    );
    if problems.is_empty() {
        return Ok(());
    }
    problems.sort();
    let listed: Vec<&str> = problems.into_iter().map(String::as_str).collect();
    Err(cuenv_core::Error::configuration(format!(
        "cuenv up: the services cannot start:\n  {}",
        listed.join("\n  ")
    )))
}

struct DependencyPlanner<'dependency_planner> {
    tasks: &'dependency_planner HashMap<String, TaskNode>,
    images: &'dependency_planner HashMap<String, ContainerImage>,
    all_services: &'dependency_planner HashMap<String, Service>,
    task_index: Tasks,
    seen_tasks: HashSet<String>,
    seen_images: HashSet<String>,
    plan: ServiceDependencyPlan,
    unknown: Vec<String>,
}

impl<'dependency_planner> DependencyPlanner<'dependency_planner> {
    fn new(
        tasks: &'dependency_planner HashMap<String, TaskNode>,
        images: &'dependency_planner HashMap<String, ContainerImage>,
        all_services: &'dependency_planner HashMap<String, Service>,
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
struct ServiceTaskContext<'context> {
    /// The project whose services are brought up.
    project: Project,
    /// Dependencies inferred from its tasks' output references.
    output_ref_deps: Vec<OutputRefDep>,
    environment: Option<&'context str>,
    project_root: &'context Path,
    cue_module_root: Option<PathBuf>,
    executor: &'context CommandExecutor,
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

/// The tasks services depend on, and the tasks of every project those
/// depend on in turn, as one graph.
fn service_task_graph(
    task_roots: &[String],
    context: &ServiceTaskContext<'_>,
) -> cuenv_core::Result<(TaskScope, TaskGraph)> {
    let mut scope = TaskScope::new(RootProject {
        project: ScopeProject {
            root: context.project_root.to_path_buf(),
            module_root: context
                .cue_module_root
                .clone()
                .unwrap_or_else(|| context.project_root.to_path_buf()),
            manifest: Arc::new(context.project.clone()),
        },
        tasks: Tasks {
            tasks: context.project.tasks.clone(),
        },
        output_ref_deps: context.output_ref_deps.clone(),
    });
    let roots = || task_roots.iter().map(String::as_str);
    // `cuenv up` runs tasks as they are declared, in this project and in
    // the ones it depends on.
    scope.reach(
        roots(),
        &ModuleProjects::new(context.executor, WorkspaceContributors::Omit),
    );
    let graph = scope.graph(roots())?;
    Ok((scope, graph))
}

async fn execute_service_task_dependencies(
    task_roots: &[String],
    context: ServiceTaskContext<'_>,
) -> cuenv_core::Result<()> {
    let (scope, task_graph) = service_task_graph(task_roots, &context)?;

    if task_graph.task_count() == 0 {
        return Ok(());
    }

    emit_stdout!(format!(
        "cuenv up: running service task dependencies: {}",
        task_roots.join(", ")
    ));

    let environments = DependencyEnvironments::new(&scope, context.environment);
    let executor = TaskExecutor::new(ExecutorConfig {
        capture_output: OutputCapture::Capture,
        max_parallel: 0,
        continue_on_error: false,
        environment: task_dependency_environment(context.project.env.as_ref(), context.environment)
            .await?,
        task_environments: (!environments.external.is_empty())
            .then(|| Arc::new(environments) as Arc<dyn TaskEnvironments>),
        working_dir: None,
        project_root: context.project_root.to_path_buf(),
        cue_module_root: context.cue_module_root,
        cache: None,
        // Inputs are hashed below the workspace, as `cuenv task` hashes
        // them: a task may read what a task of another project wrote.
        sandbox_hasher_root: Some(projects::workspace_root(context.project_root)),
        sandbox_project_roots: BTreeMap::new(),
        external_projects: scope
            .external_projects()
            .map(|(key, project)| {
                (
                    key.to_string(),
                    ProjectExecution {
                        project_root: project.root.clone(),
                        cue_module_root: Some(project.module_root.clone()),
                        cache: None,
                    },
                )
            })
            .collect(),
    });

    executor.execute_graph(&task_graph).await?;
    Ok(())
}

/// The environment of a dependency task that belongs to another project:
/// that project's own variables, resolved the first time one of its tasks
/// runs. A task of the project being brought up runs with the shared one.
#[derive(Debug)]
struct DependencyEnvironments {
    environment: Option<String>,
    external: BTreeMap<String, (ScopeProject, OnceCell<Environment>)>,
}

impl DependencyEnvironments {
    fn new(scope: &TaskScope, environment: Option<&str>) -> Self {
        Self {
            environment: environment.map(str::to_string),
            external: scope
                .external_projects()
                .map(|(key, project)| (key.to_string(), (project.clone(), OnceCell::new())))
                .collect(),
        }
    }
}

impl TaskEnvironments for DependencyEnvironments {
    fn environment_for<'a>(&'a self, name: &'a str, _task: &'a Task) -> TaskEnvironmentFuture<'a> {
        Box::pin(async move {
            let Some(external) = ExternalTaskName::parse(name) else {
                return Ok(None);
            };
            let Some((project, environment)) = self.external.get(external.project) else {
                return Err(cuenv_core::Error::configuration(format!(
                    "Task '{name}' belongs to project '{}', which this run has not loaded",
                    external.project
                )));
            };
            let environment = environment
                .get_or_try_init(|| {
                    task_dependency_environment(
                        project.manifest.env.as_ref(),
                        self.environment.as_deref(),
                    )
                })
                .await?;
            Ok(Some(environment.clone()))
        })
    }
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
    fn a_problem_of_a_service_about_to_start_stops_cuenv_up() {
        let mut project = Project::new("app");
        project.diagnostics.services.insert(
            "api".to_string(),
            vec!["service 'api': dependsOn[0] is task 'build' of project 'content'".to_string()],
        );
        project.diagnostics.images.insert(
            "web".to_string(),
            vec!["image 'web': dependsOn[0] is image 'base' of project 'content'".to_string()],
        );
        let services = |name: &str| HashMap::from([(name.to_string(), Service::default())]);
        let plan = ServiceDependencyPlan::default();

        assert!(reject_reached_problems(&project, &services("db"), &plan).is_ok());
        let Err(error) = reject_reached_problems(&project, &services("api"), &plan) else {
            panic!("a service with a problem");
        };
        assert!(error.to_string().contains("service 'api'"), "{error}");

        let reaching_image = ServiceDependencyPlan {
            image_roots: vec!["web".to_string()],
            ..ServiceDependencyPlan::default()
        };
        let Err(error) = reject_reached_problems(&project, &services("db"), &reaching_image) else {
            panic!("an image with a problem");
        };
        assert!(error.to_string().contains("image 'web'"), "{error}");
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

    /// `app`: a task that consumes an output of `content` and records the
    /// variables it could see. Commands use shell builtins only: a task's
    /// `PATH` holds nothing else in the Nix build sandbox.
    const APP: &str = r#"package cuenv
name: "app"
env: APP_VALUE: "app-own"
tasks: prepare: {
	command: "sh"
	args: ["-c", "IFS= read -r line < vendor/one.md && printf '%s\\n' \"$line\" > seen.md && printf '%s|%s' \"${APP_VALUE:-unset}\" \"${CONTENT_VALUE:-unset}\" > env.txt"]
	inputs: [{project: "content", task: "articles", map: [{from: "rendered.md", to: "vendor/one.md"}]}]
	outputs: ["seen.md", "env.txt"]
}
"#;

    /// `content`: renders an article and records the variables it could see.
    const CONTENT: &str = r#"package cuenv
name: "content"
env: CONTENT_VALUE: "content-own"
tasks: articles: {
	command: "sh"
	args: ["-c", "IFS= read -r line < articles/one.md && printf '%s\\n' \"$line\" > rendered.md && printf '%s|%s' \"${CONTENT_VALUE:-unset}\" \"${APP_VALUE:-unset}\" > rendered-env.txt"]
	inputs: ["articles/**"]
	outputs: ["rendered.md", "rendered-env.txt"]
}
"#;

    /// A module whose `app` has a task that consumes an output of `content`.
    fn module_with_a_dependency_in_another_project()
    -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
        // CUE ignores directories whose name starts with a dot.
        let directory = tempfile::Builder::new().prefix("cuenv_up_").tempdir()?;
        let write = |relative: &str, content: &str| {
            let path = directory.path().join(relative);
            path.parent().map(std::fs::create_dir_all).transpose()?;
            std::fs::write(path, content)
        };
        write(
            "cue.mod/module.cue",
            "module: \"test.example/up\"\nlanguage: version: \"v0.14.0\"\n",
        )?;
        write("app/env.cue", APP)?;
        write("content/env.cue", CONTENT)?;
        write("content/articles/one.md", "one\n")?;
        Ok(directory)
    }

    #[tokio::test]
    async fn a_service_dependency_in_another_project_runs_in_that_project()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = module_with_a_dependency_in_another_project()?;
        let root = directory.path().canonicalize()?;
        let app = root.join("app");
        let executor = CommandExecutor::new("cuenv".to_string());
        let (project, output_ref_deps) = {
            let module = executor.module(&app)?;
            let instance = module
                .get(Path::new("app"))
                .ok_or("the project evaluates")?;
            (instance.to_project()?, instance.output_ref_deps.clone())
        };

        execute_service_dependencies(
            &ServiceDependencyPlan {
                task_roots: vec!["prepare".to_string()],
                image_roots: Vec::new(),
            },
            ServiceTaskContext {
                project,
                output_ref_deps,
                environment: None,
                project_root: &app,
                cue_module_root: Some(root.clone()),
                executor: &executor,
            },
        )
        .await?;

        // The dependency ran first, in its own project, with its own
        // variables and none of the service project's.
        assert_eq!(
            std::fs::read_to_string(root.join("content/rendered-env.txt"))?,
            "content-own|unset"
        );
        // The service project's task read what it produced.
        assert_eq!(std::fs::read_to_string(app.join("seen.md"))?, "one\n");
        assert_eq!(
            std::fs::read_to_string(app.join("env.txt"))?,
            "app-own|unset"
        );
        Ok(())
    }
}
