//! [`cuenv_task_exec::TaskBackend`] implementation backed by the Dagger 1.0 Engine.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Component, Path};
use std::sync::Arc;
use std::time::Instant;

use cuenv_core::environment::Environment;
use cuenv_core::manifest::{DaggerRuntime, Entrypoint, Readiness, Runtime, Service, ServicePort};
use cuenv_core::{Error, Result};
use cuenv_task_exec::{
    BackendAttemptFuture, BackendAttemptRequest, DependencyKind, Task, TaskAttempt, TaskBackend,
    TaskCommandExt, TaskResult, literal_path_prefix,
};
use tracing::debug;

use crate::client::{
    BoundSecret, BoundService, ContainerExecRequest, ContainerSource, DaggerClient,
    DaggerInDaggerPolicy, ExportDirectoryRequest, HostDirectoryRequest, MountedCacheVolume,
    ServiceBuildRequest,
};
use crate::config::BackendSelection;
use crate::error::DaggerError;
use crate::session::DaggerSession;

mod docker_context;

const DEFAULT_CONTAINER_WORKDIR: &str = "/work";
const DEFAULT_BASE_IMAGE: &str = "debian:bookworm-slim";

/// Configuration for [`DaggerTaskBackend`].
#[derive(Debug, Clone)]
pub struct DaggerBackendConfig {
    /// Shared Dagger Engine session reused across all tasks in the run.
    pub session: Arc<DaggerSession>,
    /// Controls which tasks are routed to Dagger vs left on the host.
    pub selection: BackendSelection,
    /// Fallback OCI image when [`BackendSelection::ForceAll`] runs a task
    /// without a container or Dagger runtime.
    pub default_image: String,
}

impl DaggerBackendConfig {
    /// Create a runtime-driven backend config with the default base image.
    #[must_use]
    pub fn runtime_driven(session: Arc<DaggerSession>) -> Self {
        Self {
            session,
            selection: BackendSelection::RuntimeDriven,
            default_image: DEFAULT_BASE_IMAGE.to_string(),
        }
    }
}

/// Executes cuenv tasks inside containers via a shared [`DaggerSession`].
#[derive(Debug)]
pub struct DaggerTaskBackend {
    config: DaggerBackendConfig,
}

impl DaggerTaskBackend {
    /// Construct a new [`DaggerTaskBackend`].
    #[must_use]
    pub const fn new(config: DaggerBackendConfig) -> Self {
        Self { config }
    }

    /// Return the underlying [`DaggerSession`].
    #[must_use]
    pub fn session(&self) -> &Arc<DaggerSession> {
        &self.config.session
    }

    async fn run_attempt(&self, request: BackendAttemptRequest<'_>) -> Result<TaskAttempt> {
        let client = self.config.session.client().map_err(Error::from)?;
        let resolved_runtime = self.resolve_runtime(request.name, request.runtime)?;

        let container_workdir =
            compute_container_workdir(&resolved_runtime.workdir, request.workspace_workdir)?;
        let args = build_container_command(request.task)?;
        validate_output_mounts(&resolved_runtime, &container_workdir, &request.task.outputs)?;
        let docker_excludes = resolved_runtime
            .dockerfile
            .as_deref()
            .map(|dockerfile| {
                let path = Path::new(dockerfile);
                validate_relative_path(path, "Dockerfile path")?;
                docker_context::excludes(request.project_root, path)
            })
            .transpose()?;
        let workspace_dir_id = import_directory(&client, request.workspace_root, &[]).await?;

        let resolved_caches = client
            .resolve_cache_volumes(&resolved_runtime.cache_volumes)
            .await
            .map_err(Error::from)?;
        let mounted_caches: Vec<MountedCacheVolume<'_>> = resolved_caches
            .iter()
            .map(|(id, vol)| MountedCacheVolume {
                cache_id: id.as_str(),
                path: vol.path.as_str(),
                sharing: vol.sharing,
            })
            .collect();

        let (plain_env, secret_pairs) = partition_env(&client, request.environment).await?;
        let bound_secrets: Vec<BoundSecret<'_>> = secret_pairs
            .iter()
            .map(|(name, id)| BoundSecret {
                name: name.as_str(),
                secret_id: id.as_str(),
            })
            .collect();

        let service_pairs = resolve_dagger_services(&client, &request).await?;
        let bound_services: Vec<BoundService<'_>> = service_pairs
            .iter()
            .map(|(alias, id)| BoundService {
                alias: alias.as_str(),
                service_id: id.as_str(),
            })
            .collect();

        let cmd_str = format_command_summary(request.task);
        cuenv_events::emit_task_started!(request.name, cmd_str, false);
        let started = Instant::now();

        let docker_context_id = if let Some(exclude) = &docker_excludes {
            Some(import_directory(&client, request.project_root, exclude).await?)
        } else {
            None
        };
        let source = match (&resolved_runtime.image, &resolved_runtime.dockerfile) {
            (Some(image), None) => ContainerSource::Image(image),
            (None, Some(dockerfile)) => ContainerSource::Dockerfile {
                context_dir_id: docker_context_id.as_deref().ok_or_else(|| {
                    Error::configuration("Dagger Dockerfile build is missing its context")
                })?,
                dockerfile,
            },
            _ => {
                return Err(DaggerError::Configuration(format!(
                    "task '{}' Dagger runtime must specify exactly one of image or dockerfile",
                    request.name
                ))
                .into());
            }
        };

        let exec_future = client.execute_container(ContainerExecRequest {
            source,
            platform: resolved_runtime.platform.as_deref(),
            workspace_dir_id: &workspace_dir_id,
            workspace_path: &resolved_runtime.workdir,
            workdir: &container_workdir,
            env: &plain_env,
            secrets: &bound_secrets,
            caches: &mounted_caches,
            services: &bound_services,
            args: &args,
            dagger_in_dagger: dagger_in_dagger_policy(&resolved_runtime),
        });

        let exec_output = match request.timeout {
            Some(duration) => {
                if let Ok(result) = tokio::time::timeout(duration, exec_future).await {
                    result.map_err(Error::from)?
                } else {
                    let duration_ms =
                        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    let stderr = format!(
                        "Task '{}' timed out after {}s on Dagger backend",
                        request.name,
                        duration.as_secs()
                    );
                    cuenv_events::emit_task_output!(request.name, "stderr", stderr.clone());
                    cuenv_events::emit_task_completed!(
                        request.name,
                        false,
                        None::<i32>,
                        duration_ms
                    );
                    return Ok(TaskAttempt::TimedOut(TaskResult {
                        name: request.name.to_string(),
                        exit_code: None,
                        stdout: String::new(),
                        stderr,
                        success: false,
                    }));
                }
            }
            None => exec_future.await.map_err(Error::from)?,
        };

        let success = exec_output.exit_code == 0;
        emit_output_lines(request.name, "stdout", &exec_output.stdout);
        emit_output_lines(request.name, "stderr", &exec_output.stderr);

        if success && !request.task.outputs.is_empty() {
            let container_id = exec_output.container_id.as_deref().ok_or_else(|| {
                Error::execution("successful Dagger execution returned no container to export")
            })?;
            export_task_outputs(
                &client,
                OutputProjection {
                    container_id,
                    container_workdir: &container_workdir,
                    host_workdir: request.workdir,
                    outputs: &request.task.outputs,
                },
            )
            .await?;
        }

        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        cuenv_events::emit_task_completed!(
            request.name,
            success,
            Some(exec_output.exit_code),
            duration_ms
        );

        Ok(TaskAttempt::Completed(TaskResult {
            name: request.name.to_string(),
            exit_code: Some(exec_output.exit_code),
            stdout: exec_output.stdout,
            stderr: exec_output.stderr,
            success,
        }))
    }

    fn resolve_runtime(&self, task_name: &str, runtime: Option<&Runtime>) -> Result<DaggerRuntime> {
        match runtime {
            Some(Runtime::Dagger(dagger)) => Ok((**dagger).clone()),
            Some(Runtime::Container(container)) => Ok(DaggerRuntime {
                image: Some(container.image.clone()),
                workdir: DEFAULT_CONTAINER_WORKDIR.to_string(),
                ..DaggerRuntime::default()
            }),
            _ if self.config.selection == BackendSelection::ForceAll => Ok(DaggerRuntime {
                image: Some(self.config.default_image.clone()),
                workdir: DEFAULT_CONTAINER_WORKDIR.to_string(),
                ..DaggerRuntime::default()
            }),
            _ => Err(DaggerError::Configuration(format!(
                "task '{task_name}' has no container or dagger runtime configured"
            ))
            .into()),
        }
    }
}

impl TaskBackend for DaggerTaskBackend {
    fn name(&self) -> &'static str {
        "dagger"
    }

    fn manages_own_cache(&self) -> bool {
        true
    }

    fn handles_task(&self, _task: &Task, runtime: Option<&Runtime>) -> bool {
        match self.config.selection {
            BackendSelection::ForceAll => true,
            BackendSelection::RuntimeDriven => {
                matches!(runtime, Some(Runtime::Container(_) | Runtime::Dagger(_)))
            }
        }
    }

    fn execute_attempt<'a>(
        &'a self,
        request: BackendAttemptRequest<'a>,
    ) -> BackendAttemptFuture<'a> {
        Box::pin(async move { self.run_attempt(request).await })
    }
}

async fn partition_env(
    client: &DaggerClient,
    environment: &Environment,
) -> Result<(BTreeMap<String, String>, Vec<(String, String)>)> {
    let mut plain = BTreeMap::new();
    let mut secrets = Vec::new();

    for (key, value) in environment.iter() {
        if environment.is_secret_var(key) {
            let secret_id = client.set_secret(key, value).await.map_err(Error::from)?;
            secrets.push((key.clone(), secret_id));
        } else {
            plain.insert(key.clone(), value.clone());
        }
    }

    secrets.sort_unstable_by(|first, second| first.0.cmp(&second.0));
    Ok((plain, secrets))
}

async fn resolve_dagger_services(
    client: &DaggerClient,
    request: &BackendAttemptRequest<'_>,
) -> Result<Vec<(String, String)>> {
    let requested: Vec<&str> = request.task.service_dependencies().collect();
    if requested.is_empty() {
        return Ok(Vec::new());
    }

    let ordered = collect_services(request.services, &requested)?;
    let mut ids = HashMap::with_capacity(ordered.len());

    for (name, service) in ordered {
        let spec = service_container_spec(name, service)?;
        let args = service_entrypoint_args(service)?;
        let ports = service_ports(name, service)?;
        let (values, secrets) =
            Environment::resolve_for_service_with_secrets(name, &service.env).await?;
        cuenv_events::register_secrets(secrets);
        let mut environment = Environment::new();
        for (key, value) in values {
            if service
                .env
                .get(&key)
                .is_some_and(|env| !env.collect_secrets().is_empty())
            {
                environment.set_secret(key, value);
            } else {
                environment.set(key, value);
            }
        }
        let (env, secrets) = partition_env(client, &environment).await?;
        let secret_refs: Vec<BoundSecret<'_>> = secrets
            .iter()
            .map(|(name, id)| BoundSecret {
                name: name.as_str(),
                secret_id: id.as_str(),
            })
            .collect();
        let dependencies: Vec<BoundService<'_>> = service
            .depends_on
            .iter()
            .filter_map(|dep| {
                ids.get(dep.name.as_str()).map(|id: &String| BoundService {
                    alias: &dep.name,
                    service_id: id,
                })
            })
            .collect();

        debug!(service = %name, image = %spec.image, "building Dagger service");
        let id = client
            .build_service(ServiceBuildRequest {
                image: spec.image,
                platform: spec.platform,
                workdir: service.dir.as_deref().or(spec.workdir),
                dagger_in_dagger: spec.dagger_in_dagger,
                args: &args,
                exposed_ports: &ports,
                env: &env,
                secrets: &secret_refs,
                services: &dependencies,
            })
            .await
            .map_err(Error::from)?;
        ids.insert(name, id);
    }

    requested
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|name| {
            let id = ids.remove(name).ok_or_else(|| {
                Error::configuration(format!("Dagger service '{name}' could not be resolved"))
            })?;
            Ok((name.to_string(), id))
        })
        .collect()
}

fn collect_services<'a>(
    all_services: &'a HashMap<String, Service>,
    requested: &[&'a str],
) -> Result<Vec<(&'a str, &'a Service)>> {
    enum Visit<'a> {
        Enter(&'a str),
        Leave(&'a str, &'a Service),
    }

    let mut ordered = Vec::new();
    let mut active = HashSet::new();
    let mut finished = HashSet::new();
    let mut stack: Vec<_> = requested
        .iter()
        .rev()
        .map(|&name| Visit::Enter(name))
        .collect();

    while let Some(visit) = stack.pop() {
        match visit {
            Visit::Leave(name, service) => {
                active.remove(name);
                finished.insert(name);
                ordered.push((name, service));
            }
            Visit::Enter(name) if !finished.contains(name) => {
                if !active.insert(name) {
                    return Err(Error::configuration(format!(
                        "cycle in Dagger service dependencies at '{name}'"
                    )));
                }
                let service = all_services.get(name).ok_or_else(|| {
                    Error::configuration(format!("unknown Dagger service '{name}'"))
                })?;
                stack.push(Visit::Leave(name, service));
                for dependency in service.depends_on.iter().rev() {
                    if dependency.kind != DependencyKind::Service {
                        return Err(Error::configuration(format!(
                            "Dagger service '{name}' cannot run task or image dependency '{}'",
                            dependency.name
                        )));
                    }
                    stack.push(Visit::Enter(&dependency.name));
                }
            }
            Visit::Enter(_) => {}
        }
    }
    Ok(ordered)
}

struct ServiceContainerSpec<'a> {
    image: &'a str,
    platform: Option<&'a str>,
    workdir: Option<&'a str>,
    dagger_in_dagger: DaggerInDaggerPolicy,
}

fn service_container_spec<'a>(
    name: &str,
    service: &'a Service,
) -> Result<ServiceContainerSpec<'a>> {
    match service.runtime.as_ref() {
        Some(Runtime::Container(container)) => Ok(ServiceContainerSpec {
            image: &container.image,
            platform: None,
            workdir: None,
            dagger_in_dagger: DaggerInDaggerPolicy::Allow,
        }),
        Some(Runtime::Dagger(dagger)) => {
            let image = dagger.image.as_deref().ok_or_else(|| {
                DaggerError::Configuration(format!(
                    "service '{name}' uses a Dagger runtime without an image; Dockerfile services are not supported"
                ))
            })?;
            Ok(ServiceContainerSpec {
                image,
                platform: dagger.platform.as_deref(),
                workdir: Some(&dagger.workdir),
                dagger_in_dagger: dagger_in_dagger_policy(dagger),
            })
        }
        _ => Err(DaggerError::Configuration(format!(
            "service '{name}' must declare a container or dagger runtime to be bound on the Dagger backend"
        ))
        .into()),
    }
}

fn dagger_in_dagger_policy(runtime: &DaggerRuntime) -> DaggerInDaggerPolicy {
    if runtime.disable_dagger_in_dagger {
        DaggerInDaggerPolicy::Disable
    } else {
        DaggerInDaggerPolicy::Allow
    }
}

fn service_ports(name: &str, service: &Service) -> Result<Vec<ServicePort>> {
    let mut ports = service.ports.clone();
    if let Some(readiness) = &service.readiness {
        let Readiness::Port(readiness) = readiness else {
            return Err(Error::configuration(format!(
                "Dagger service '{name}' supports port readiness; configure other checks in its image"
            )));
        };
        if readiness
            .host
            .as_deref()
            .is_some_and(|host| !matches!(host, "127.0.0.1" | "localhost" | "::1"))
        {
            return Err(Error::configuration(format!(
                "Dagger service '{name}' must check a port on its own container"
            )));
        }
        if !ports
            .iter()
            .any(|port| port.port == readiness.port && port.protocol.eq_ignore_ascii_case("tcp"))
        {
            ports.push(ServicePort {
                protocol: "tcp".to_string(),
                port: readiness.port,
                name: None,
            });
        }
    }
    Ok(ports)
}

fn service_entrypoint_args(service: &Service) -> Result<Vec<String>> {
    match &service.entrypoint {
        Entrypoint::Task(task) => build_container_command(task),
        Entrypoint::Script(script) => build_container_command(&Task {
            script: Some(script.script.clone()),
            script_shell: script.script_shell,
            shell_options: script.shell_options,
            ..Task::default()
        }),
        Entrypoint::Command(cmd) => {
            if cmd.command.is_empty() {
                return Ok(Vec::new());
            }
            let mut args = Vec::with_capacity(cmd.args.len() + 1);
            args.push(cmd.command.clone());
            for arg in &cmd.args {
                let arg = arg.as_str().ok_or_else(|| {
                    Error::configuration("Dagger service entrypoint has an unresolved argument")
                })?;
                args.push(arg.to_string());
            }
            Ok(args)
        }
    }
}

fn build_container_command(task: &Task) -> Result<Vec<String>> {
    let mut spec = task.command_spec(str::to_string)?;
    if spec.program.is_empty() {
        return Err(Error::configuration(
            "a Dagger task must specify an executable or script",
        ));
    }
    spec.args.insert(0, spec.program);
    Ok(spec.args)
}

async fn import_directory(
    client: &DaggerClient,
    root: &Path,
    exclude: &[String],
) -> Result<String> {
    let path = root.to_str().ok_or_else(|| {
        Error::configuration(format!("non-UTF-8 workspace path: {}", root.display()))
    })?;
    client
        .host_directory(HostDirectoryRequest {
            path,
            include: &[],
            exclude,
        })
        .await
        .map_err(Error::from)
}

fn compute_container_workdir(base: &str, relative: &Path) -> Result<String> {
    if !base.starts_with('/') || base.split('/').any(|part| part == "..") {
        return Err(Error::configuration(format!(
            "Dagger runtime workdir must be an absolute container directory: {base}"
        )));
    }
    validate_relative_path(relative, "task working directory")?;
    let relative = relative.to_str().ok_or_else(|| {
        Error::configuration(format!("non-UTF-8 task directory: {}", relative.display()))
    })?;
    if relative.is_empty() {
        return Ok(base.to_string());
    }
    Ok(format!("{}/{relative}", base.trim_end_matches('/')))
}

fn validate_relative_path(path: &Path, label: &str) -> Result<()> {
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(Error::configuration(format!(
            "{label} must stay inside its workspace: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_output_mounts(
    runtime: &DaggerRuntime,
    workdir: &str,
    outputs: &[String],
) -> Result<()> {
    for output in outputs {
        let output = output.trim();
        validate_relative_path(Path::new(output), "output pattern")?;
        let prefix = literal_path_prefix(output);
        let path = Path::new(workdir).join(prefix);
        for cache in &runtime.cache_volumes {
            let cache = Path::new(workdir).join(&cache.path);
            if path.starts_with(&cache) {
                return Err(Error::configuration(format!(
                    "Dagger output '{output}' is inside cache mount '{}'; write declared outputs outside cache mounts",
                    cache.display()
                )));
            }
        }
    }
    Ok(())
}

struct OutputProjection<'a> {
    container_id: &'a str,
    container_workdir: &'a str,
    host_workdir: &'a Path,
    outputs: &'a [String],
}

async fn export_task_outputs(client: &DaggerClient, output: OutputProjection<'_>) -> Result<()> {
    let OutputProjection {
        container_id,
        container_workdir,
        host_workdir,
        outputs,
    } = output;
    let Some(include) = output_prefixes(outputs) else {
        return Ok(());
    };
    let staged = tempfile::tempdir().map_err(|error| {
        Error::io_with_path(
            "create Dagger output staging directory",
            std::env::temp_dir(),
            error,
        )
    })?;
    let staged_path = staged.path().to_str().ok_or_else(|| {
        Error::configuration("Dagger output staging directory is not valid UTF-8")
    })?;
    client
        .export_directory(ExportDirectoryRequest {
            container_id,
            container_path: container_workdir,
            host_path: staged_path,
            include: &include,
        })
        .await
        .map_err(Error::from)?;
    cuenv_task_exec::exec_root::project_declared_outputs(staged.path(), host_workdir, outputs)
}

// Limit transfer to the literal subtree(s) containing possible matches. The
// local output collector applies cuenv's glob semantics and validates the
// snapshot before changing the live workspace.
fn output_prefixes(outputs: &[String]) -> Option<Vec<String>> {
    let mut prefixes = BTreeSet::new();
    let outputs = outputs
        .iter()
        .map(|output| output.trim())
        .filter(|output| !output.is_empty());
    for output in outputs {
        let prefix = literal_path_prefix(output);
        if prefix.is_empty() || prefix.starts_with('!') || prefix.contains('\\') {
            return Some(Vec::new());
        }
        prefixes.insert(prefix);
    }
    (!prefixes.is_empty()).then(|| prefixes.into_iter().collect())
}

fn format_command_summary(task: &Task) -> String {
    if let Some(script) = &task.script {
        format!("[dagger script: {} bytes]", script.len())
    } else if task.command.is_empty() {
        format!("[dagger] {}", task.args.join(" "))
    } else if task.args.is_empty() {
        format!("[dagger] {}", task.command)
    } else {
        format!("[dagger] {} {}", task.command, task.args.join(" "))
    }
}

fn emit_output_lines(task_name: &str, stream: &str, body: &str) {
    for line in body.lines() {
        cuenv_events::emit_task_output!(task_name, stream, line.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::OutputCapture;
    use cuenv_manifest::tasks::TaskDependency;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;

    use crate::session::SessionEndpoint;

    async fn spawn_mock_dagger_server(
        exit_code: i32,
    ) -> std::io::Result<(SessionEndpoint, Arc<Mutex<Vec<serde_json::Value>>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let recorded_bg = Arc::clone(&recorded);

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let recorded_req = Arc::clone(&recorded_bg);
                tokio::spawn(async move {
                    let mut buf = vec![0_u8; 65536];
                    let Ok(n) = socket.read(&mut buf).await else {
                        return;
                    };
                    let raw = String::from_utf8_lossy(&buf[..n]);
                    let body = raw.split("\r\n\r\n").nth(1).unwrap_or("");
                    let payload: serde_json::Value =
                        serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
                    let query = payload
                        .get("query")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let vars = payload["variables"].clone();
                    recorded_req.lock().await.push(payload);

                    let response_json = if query.contains("host { directory") {
                        serde_json::json!({ "data": { "host": { "directory": { "id": "dir-1" } } } })
                    } else if query.contains("cacheVolume") {
                        serde_json::json!({ "data": { "cacheVolume": { "id": "cache-1" } } })
                    } else if query.contains("setSecret") {
                        serde_json::json!({ "data": { "setSecret": { "id": "secret-1" } } })
                    } else if query.contains("asService") {
                        let id = if vars["image"] == "postgres:16" {
                            "db-1"
                        } else {
                            "svc-1"
                        };
                        serde_json::json!({ "data": { "container": { "from": { "asService": { "id": id } } } } })
                    } else if query.contains("withExec") && exit_code != 0 {
                        serde_json::json!({
                            "errors": [{
                                "message": "process exited unsuccessfully",
                                "extensions": {
                                    "_type": "EXEC_ERROR",
                                    "exitCode": exit_code,
                                    "stdout": "",
                                    "stderr": "task failed"
                                }
                            }]
                        })
                    } else if query.contains("withExec") {
                        serde_json::json!({
                            "data": {
                                "container": {
                                    "from": {
                                        "withExec": {
                                            "id": "ctr-1",
                                            "stdout": "hello from dagger\n",
                                            "stderr": "",
                                            "exitCode": exit_code
                                        }
                                    }
                                }
                            }
                        })
                    } else if query.contains("filter(include:") {
                        let Some(dest) = vars["dst"].as_str().map(Path::new) else {
                            return;
                        };
                        let target = dest.join("target");
                        if std::fs::create_dir_all(&target)
                            .and_then(|()| std::fs::write(target.join("report.txt"), "report"))
                            .and_then(|()| std::fs::write(target.join("unowned.bin"), "unowned"))
                            .is_err()
                        {
                            return;
                        }
                        serde_json::json!({
                            "data": { "node": { "directory": { "filter": { "export": dest } } } }
                        })
                    } else {
                        serde_json::json!({ "errors": [{ "message": format!("unexpected query: {query}") }] })
                    };

                    let resp_body = response_json.to_string();
                    let status = if exit_code != 0 && query.contains("withExec") {
                        "422 Unprocessable Content"
                    } else {
                        "200 OK"
                    };
                    let http = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        resp_body.len(),
                        resp_body
                    );
                    let _ = socket.write_all(http.as_bytes()).await;
                });
            }
        });

        Ok((
            SessionEndpoint {
                port,
                session_token: "test-token".to_string(),
            },
            recorded,
        ))
    }

    #[tokio::test]
    async fn executes_dagger_task_with_caches_secrets_services_and_outputs()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (endpoint, recorded) = spawn_mock_dagger_server(0).await?;
        let session = Arc::new(DaggerSession::from_endpoint(endpoint));
        let backend = DaggerTaskBackend::new(DaggerBackendConfig::runtime_driven(session));

        let dir = tempfile::tempdir()?;
        let cwd = dir.path().join("subdir");
        std::fs::create_dir_all(cwd.join("target"))?;
        std::fs::write(cwd.join("target/old.txt"), "old report")?;
        std::fs::write(cwd.join("target/keep.bin"), "keep")?;
        let mut task: Task = serde_json::from_value(serde_json::json!({
            "command": "cargo",
            "args": ["test"],
            "inputs": ["src/**"],
            "outputs": ["target/*.txt"],
            "runtime": {
                "type": "dagger",
                "image": "rust:1.88",
                "workdir": "/work",
                "cacheVolumes": [
                    { "key": "cargo-registry", "path": "/usr/local/cargo/registry", "sharing": "SHARED" }
                ]
            }
        }))?;
        task.add_typed_dependency(TaskDependency::from_service_name("app"));

        let pg: Service = serde_json::from_value(serde_json::json!({
            "type": "service",
            "runtime": { "type": "container", "image": "postgres:16" },
            "ports": [{ "port": 5432 }],
            "env": { "POSTGRES_PASSWORD": "test" }
        }))?;
        let app: Service = serde_json::from_value(serde_json::json!({
            "type": "service",
            "runtime": { "type": "container", "image": "app:test" },
            "dependsOn": [{"_name": "postgres", "kind": "service"}],
        }))?;
        let services = HashMap::from([("postgres".to_string(), pg), ("app".to_string(), app)]);

        let mut env = Environment::new();
        env.set("PLAIN_VAR".to_string(), "value".to_string());
        env.set_secret("API_TOKEN".to_string(), "super-secret".to_string());

        let outcome = backend
            .execute_attempt(BackendAttemptRequest {
                name: "integration",
                task: &task,
                environment: &env,
                project_root: dir.path(),
                workdir: &cwd,
                workspace_root: dir.path(),
                workspace_workdir: Path::new("subdir"),
                runtime: task.runtime.as_ref(),
                services: &services,
                timeout: None,
                capture_output: OutputCapture::Capture,
            })
            .await?;

        let TaskAttempt::Completed(result) = outcome else {
            panic!("expected completed attempt");
        };
        assert!(result.success);
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.stdout, "hello from dagger\n");

        let queries = recorded.lock().await;
        let query_strings: Vec<&str> = queries
            .iter()
            .filter_map(|v| v.get("query").and_then(serde_json::Value::as_str))
            .collect();
        assert!(query_strings.iter().any(|q| q.contains("host { directory")));
        assert!(query_strings.iter().any(|q| q.contains("cacheVolume")));
        assert!(query_strings.iter().any(|q| q.contains("setSecret")));
        assert!(query_strings.iter().any(|q| q.contains("asService")));
        assert!(query_strings.iter().any(|q| {
            q.contains("withMountedCache")
                && q.contains("withServiceBinding")
                && q.contains("withSecretVariable")
                && !q.contains("expect: ANY")
        }));
        let app = queries
            .iter()
            .find(|query| query["variables"]["image"] == "app:test")
            .ok_or("app service request missing")?;
        assert_eq!(app["variables"]["sa0"], "postgres");
        assert_eq!(app["variables"]["svcid0"], "db-1");
        let exec = queries
            .iter()
            .find(|query| query["variables"]["workspacePath"] == "/work")
            .ok_or("task exec request missing")?;
        assert_eq!(exec["variables"]["workdir"], "/work/subdir");
        assert_eq!(exec["variables"]["sa0"], "app");
        assert_eq!(exec["variables"]["svcid0"], "svc-1");
        let export = queries
            .iter()
            .find(|query| query["variables"].get("dst").is_some())
            .ok_or("output export request missing")?;
        assert_eq!(export["variables"]["src"], "/work/subdir");
        assert_eq!(
            export["variables"]["include"],
            serde_json::json!(["target"])
        );
        assert_eq!(
            std::fs::read_to_string(cwd.join("target/report.txt"))?,
            "report"
        );
        assert!(!cwd.join("target/old.txt").exists());
        assert!(!cwd.join("target/unowned.bin").exists());
        assert!(cwd.join("target/keep.bin").exists());
        Ok(())
    }

    #[tokio::test]
    async fn non_zero_exit_code_does_not_export_outputs()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (endpoint, recorded) = spawn_mock_dagger_server(42).await?;
        let session = Arc::new(DaggerSession::from_endpoint(endpoint));
        let backend = DaggerTaskBackend::new(DaggerBackendConfig::runtime_driven(session));

        let dir = tempfile::tempdir()?;
        let task: Task = serde_json::from_value(serde_json::json!({
            "script": "exit 42",
            "shellOptions": { "nounset": false },
            "outputs": ["dist/out.txt"],
            "runtime": { "type": "dagger", "dockerfile": "Dockerfile", "platform": "linux/amd64" }
        }))?;

        let services = HashMap::new();
        let env = Environment::new();
        let outcome = backend
            .execute_attempt(BackendAttemptRequest {
                name: "failing",
                task: &task,
                environment: &env,
                project_root: dir.path(),
                workdir: dir.path(),
                workspace_root: dir.path(),
                workspace_workdir: Path::new(""),
                runtime: task.runtime.as_ref(),
                services: &services,
                timeout: None,
                capture_output: OutputCapture::Capture,
            })
            .await?;

        let TaskAttempt::Completed(result) = outcome else {
            panic!("expected completed attempt");
        };
        assert!(!result.success);
        assert_eq!(result.exit_code, Some(42));
        assert_eq!(result.stderr, "task failed");

        let queries = recorded.lock().await;
        assert!(queries.iter().any(|request| {
            let vars = &request["variables"];
            vars["args"] == serde_json::json!(["bash", "-c", "set -e -o pipefail\nexit 42"])
                && vars["platform"] == "linux/amd64"
                && request["query"].as_str().is_some_and(|query| {
                    query.contains("node(id: $buildCtx)") && query.contains("dockerBuild(")
                })
        }));
        assert!(
            !queries
                .iter()
                .any(|request| request["variables"].get("dst").is_some())
        );
        Ok(())
    }

    #[test]
    fn rejects_service_dependency_cycles() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let a: Service = serde_json::from_value(serde_json::json!({
            "dependsOn": [{"_name": "b", "kind": "service"}],
        }))?;
        let b: Service = serde_json::from_value(serde_json::json!({
            "dependsOn": [{"_name": "a", "kind": "service"}],
        }))?;
        let services = HashMap::from([("a".to_string(), a), ("b".to_string(), b)]);
        assert!(
            collect_services(&services, &["a"])
                .is_err_and(|error| error.to_string().contains("cycle"))
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires a Dagger 1.0 engine and access to Alpine and BusyBox images"]
    async fn live_engine_runs_nested_services_and_exports_declared_outputs()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("project");
        let staged = dir.path().join("staging");
        let cwd = project.join("subdir");
        std::fs::create_dir_all(&cwd)?;
        std::fs::create_dir_all(staged.join("subdir"))?;
        let dockerfile = Path::new("images/Dockerfile");
        std::fs::create_dir(project.join("images"))?;
        std::fs::create_dir(project.join("private"))?;
        std::fs::write(
            project.join(dockerfile),
            "FROM alpine:3.21\nCOPY private/allowed.txt /allowed.txt\n",
        )?;
        std::fs::write(project.join(".dockerignore"), "private\n")?;
        std::fs::write(
            project.join("images/Dockerfile.dockerignore"),
            "/private/**\n!private/allowed.txt\nimages\n",
        )?;
        std::fs::write(project.join("private/hidden.txt"), "hidden")?;
        std::fs::write(project.join("private/allowed.txt"), "allowed")?;
        std::fs::write(staged.join("subdir/input.txt"), "from staged input")?;
        let session = Arc::new(
            DaggerSession::connect(crate::session::SessionConfig {
                working_dir: Some(project.clone()),
                ..Default::default()
            })
            .await?,
        );
        // Inspect the imported host snapshot, before dockerBuild could apply
        // its separate engine-side copy filter.
        let client = session.client()?;
        let excludes = docker_context::excludes(&project, dockerfile)?;
        let context = import_directory(&client, &project, &excludes).await?;
        let directory = client
            .query(
                "query($id: ID!) { node(id: $id) { ... on Directory { \
                    hidden: exists(path: \"private/hidden.txt\") \
                    allowed: exists(path: \"private/allowed.txt\") \
                } } }",
                serde_json::json!({ "id": context }),
            )
            .await?;
        assert_eq!(directory["node"]["hidden"], false);
        assert_eq!(directory["node"]["allowed"], true);
        let backend =
            DaggerTaskBackend::new(DaggerBackendConfig::runtime_driven(Arc::clone(&session)));
        let task: Task = serde_json::from_value(serde_json::json!({
            "script": "test -f /allowed.txt && test -n \"$TOKEN\" && mkdir -p dist && wget -qO dist/check.txt http://app:8080/ && cp input.txt dist/input.txt && touch dist/ignored.bin",
            "scriptShell": "sh",
            "outputs": ["dist/*.txt"],
            "runtime": { "type": "dagger", "dockerfile": "images/Dockerfile" },
            "dependsOn": [{"_name": "app", "kind": "service"}],
        }))?;
        let db = serde_json::from_value(serde_json::json!({
            "entrypoint": {"command": "sh", "args": ["-ec", "mkdir -p /srv; echo service-ready >/srv/index.html; exec busybox httpd -f -p 8080 -h /srv"]},
            "readiness": {"kind": "port", "port": 8080},
            "runtime": { "type": "dagger", "image": "busybox:1.37.0", "disableDaggerInDagger": true },
        }))?;
        let app = serde_json::from_value(serde_json::json!({
            "entrypoint": {"command": "sh", "args": ["-ec", "mkdir -p /srv; wget -qO /srv/index.html http://db:8080/; exec busybox httpd -f -p 8080 -h /srv"]},
            "dependsOn": [{"_name": "db", "kind": "service"}],
            "ports": [{"port": 8080}],
            "runtime": { "type": "container", "image": "busybox:1.37.0" },
        }))?;
        let services = HashMap::from([("db".to_string(), db), ("app".to_string(), app)]);
        let mut env = Environment::new();
        env.set_secret("TOKEN".to_owned(), "synthetic-test-token".to_owned());
        let result = backend
            .execute_attempt(BackendAttemptRequest {
                name: "smoke",
                task: &task,
                environment: &env,
                project_root: &project,
                workdir: &cwd,
                workspace_root: &staged,
                workspace_workdir: Path::new("subdir"),
                runtime: task.runtime.as_ref(),
                services: &services,
                timeout: Some(std::time::Duration::from_secs(90)),
                capture_output: OutputCapture::Capture,
            })
            .await;
        session.shutdown().await;
        let TaskAttempt::Completed(result) = result? else {
            return Err("Dagger smoke task timed out".into());
        };
        assert!(
            result.success,
            "Dagger smoke task failed: {}",
            result.stderr
        );
        assert_eq!(
            std::fs::read_to_string(cwd.join("dist/input.txt"))?,
            "from staged input"
        );
        assert_eq!(
            std::fs::read_to_string(cwd.join("dist/check.txt"))?,
            "service-ready\n"
        );
        assert!(!cwd.join("dist/ignored.bin").exists());
        Ok(())
    }
}
