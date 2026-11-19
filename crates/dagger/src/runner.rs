//! [`cuenv_task_exec::TaskBackend`] implementation backed by the Dagger 1.0 Engine.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use cuenv_core::environment::EnvValue;
use cuenv_core::manifest::{DaggerRuntime, Entrypoint, Runtime, Service};
use cuenv_core::{Error, Result};
use cuenv_task_exec::{
    BackendAttemptFuture, BackendAttemptRequest, Task, TaskAttempt, TaskBackend, TaskResult,
};
use tracing::debug;

use crate::client::{
    BoundSecret, BoundService, ContainerExecRequest, ContainerSource, DaggerClient,
    DaggerInDaggerPolicy, ExportPathRequest, HostDirectoryRequest, MountedCacheVolume, ScriptFile,
    ServiceBuildRequest,
};
use crate::config::BackendSelection;
use crate::error::DaggerError;
use crate::session::DaggerSession;

const DEFAULT_CONTAINER_WORKDIR: &str = "/work";
const DEFAULT_BASE_IMAGE: &str = "alpine:3.21";
const SCRIPT_CONTAINER_PATH: &str = "/tmp/cuenv-task.sh";

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

        let input_patterns: Vec<String> = request
            .task
            .inputs
            .iter()
            .filter_map(|input| match input {
                cuenv_manifest::tasks::Input::Path(p) => Some(p.clone()),
                _ => None,
            })
            .collect();

        let host_root = request.project_root.to_string_lossy().into_owned();
        let workspace_dir_id = client
            .host_directory(HostDirectoryRequest {
                path: &host_root,
                include: &input_patterns,
                exclude: &[],
            })
            .await
            .map_err(Error::from)?;

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

        let (plain_env, secret_pairs) = partition_env(&client, &request).await?;
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

        let (args, script_body) = build_container_command(request.task)?;
        let script_file = script_body.as_deref().map(|contents| ScriptFile {
            path: SCRIPT_CONTAINER_PATH,
            contents,
        });

        let cmd_str = format_command_summary(request.task);
        cuenv_events::emit_task_started!(request.name, cmd_str, false);
        let started = Instant::now();

        let source = match (&resolved_runtime.image, &resolved_runtime.dockerfile) {
            (Some(image), None) => ContainerSource::Image(image.as_str()),
            (None, Some(dockerfile)) => ContainerSource::Dockerfile {
                context_dir_id: &workspace_dir_id,
                dockerfile: dockerfile.as_str(),
            },
            _ => {
                return Err(DaggerError::Configuration(format!(
                    "task '{}' Dagger runtime must specify exactly one of `image` or `dockerfile`",
                    request.name
                ))
                .into());
            }
        };

        let dagger_in_dagger = if resolved_runtime.disable_dagger_in_dagger {
            DaggerInDaggerPolicy::Disable
        } else {
            DaggerInDaggerPolicy::Allow
        };

        let container_workdir = compute_container_workdir(
            &resolved_runtime.workdir,
            request.project_root,
            request.workdir,
        );

        let exec_future = client.execute_container(ContainerExecRequest {
            source,
            platform: resolved_runtime.platform.as_deref(),
            workspace_dir_id: &workspace_dir_id,
            workdir: &container_workdir,
            env: &plain_env,
            secrets: &bound_secrets,
            caches: &mounted_caches,
            services: &bound_services,
            script_file,
            args: &args,
            dagger_in_dagger,
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
        if success && !request.task.outputs.is_empty() {
            export_task_outputs(
                &client,
                &exec_output.container_id,
                &container_workdir,
                request.workdir,
                &request.task.outputs,
            )
            .await?;
        }

        emit_output_lines(request.name, "stdout", &exec_output.stdout);
        emit_output_lines(request.name, "stderr", &exec_output.stderr);

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
    request: &BackendAttemptRequest<'_>,
) -> Result<(BTreeMap<String, String>, Vec<(String, String)>)> {
    let mut plain = BTreeMap::new();
    let mut secrets = Vec::new();

    for (key, value) in request.environment.iter() {
        if request.environment.is_secret_var(key) {
            let secret_id = client.set_secret(key, value).await.map_err(Error::from)?;
            secrets.push((key.clone(), secret_id));
        } else {
            plain.insert(key.clone(), value.clone());
        }
    }

    for (key, value) in &request.task.env {
        if let Some(rendered) = render_json_env_value(value) {
            plain.insert(key.clone(), rendered);
        }
    }

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
    let mut bound = Vec::with_capacity(ordered.len());

    for (alias, service) in ordered {
        let (image, platform) = service_container_spec(&alias, service)?;
        let ports: Vec<u16> = service.ports.iter().map(|p| p.port).collect();
        let args = service_entrypoint_args(service);

        let mut env = BTreeMap::new();
        let mut svc_secrets = Vec::new();
        for (k, v) in &service.env {
            if let Some(rendered) = render_env_value(v) {
                if request.environment.is_secret_var(k) {
                    let id = client.set_secret(k, &rendered).await.map_err(Error::from)?;
                    svc_secrets.push((k.clone(), id));
                } else {
                    env.insert(k.clone(), rendered);
                }
            }
        }
        let secret_refs: Vec<BoundSecret<'_>> = svc_secrets
            .iter()
            .map(|(name, id)| BoundSecret {
                name: name.as_str(),
                secret_id: id.as_str(),
            })
            .collect();

        debug!(service = %alias, image = %image, ports = ?ports, "building Dagger service");
        let service_id = client
            .build_service(ServiceBuildRequest {
                image: &image,
                platform: platform.as_deref(),
                args: &args,
                exposed_ports: &ports,
                env: &env,
                secrets: &secret_refs,
            })
            .await
            .map_err(Error::from)?;
        bound.push((alias, service_id));
    }

    Ok(bound)
}

fn collect_services<'s>(
    all_services: &'s HashMap<String, Service>,
    requested: &[&str],
) -> Result<Vec<(String, &'s Service)>> {
    let mut ordered = Vec::new();
    let mut visited = HashSet::new();
    let mut stack: Vec<String> = requested.iter().map(|s| (*s).to_string()).collect();

    while let Some(name) = stack.pop() {
        if !visited.insert(name.clone()) {
            continue;
        }
        let service = all_services.get(&name).ok_or_else(|| {
            DaggerError::Configuration(format!(
                "task depends on unknown service '{name}' (not declared in project services)"
            ))
        })?;
        for dep in &service.depends_on {
            if all_services.contains_key(&dep.name) {
                stack.push(dep.name.clone());
            }
        }
        ordered.push((name, service));
    }

    Ok(ordered)
}

fn service_container_spec(name: &str, service: &Service) -> Result<(String, Option<String>)> {
    match service.runtime.as_ref() {
        Some(Runtime::Container(container)) => Ok((container.image.clone(), None)),
        Some(Runtime::Dagger(dagger)) => {
            let image = dagger.image.clone().ok_or_else(|| {
                DaggerError::Configuration(format!(
                    "service '{name}' uses a Dagger runtime without an `image` (Dockerfile services are not yet supported)"
                ))
            })?;
            Ok((image, dagger.platform.clone()))
        }
        _ => Err(DaggerError::Configuration(format!(
            "service '{name}' must declare a `container` or `dagger` runtime to be bound on the Dagger backend"
        ))
        .into()),
    }
}

fn service_entrypoint_args(service: &Service) -> Vec<String> {
    match &service.entrypoint {
        Entrypoint::Command(cmd) => {
            if cmd.command.is_empty() {
                return Vec::new();
            }
            let mut args = vec![cmd.command.clone()];
            for arg in &cmd.args {
                if let Some(s) = arg.as_str() {
                    args.push(s.to_string());
                } else {
                    args.push(arg.to_string());
                }
            }
            args
        }
        Entrypoint::Script(script) => {
            vec!["sh".to_string(), "-euc".to_string(), script.script.clone()]
        }
        Entrypoint::Task(task) => {
            if let Some(script) = &task.script {
                vec!["sh".to_string(), "-euc".to_string(), script.clone()]
            } else if !task.command.is_empty() {
                let mut args = vec![task.command.clone()];
                args.extend(task.args.clone());
                args
            } else {
                Vec::new()
            }
        }
    }
}

fn build_container_command(task: &Task) -> Result<(Vec<String>, Option<String>)> {
    if let Some(script) = &task.script {
        let shell = task
            .script_shell
            .as_ref()
            .map_or_else(|| "sh".to_string(), |s| s.command_and_flag().0.to_string());
        let args = vec![shell, SCRIPT_CONTAINER_PATH.to_string()];
        return Ok((args, Some(script.clone())));
    }

    if task.command.is_empty() && task.args.is_empty() {
        return Err(DaggerError::Configuration(
            "task must specify either `command` or `script`".to_string(),
        )
        .into());
    }

    let mut args = Vec::with_capacity(task.args.len().saturating_add(1));
    if !task.command.is_empty() {
        args.push(task.command.clone());
    }
    args.extend(task.args.clone());
    Ok((args, None))
}

fn compute_container_workdir(
    base_workdir: &str,
    project_root: &Path,
    host_workdir: &Path,
) -> String {
    if let Ok(rel) = host_workdir.strip_prefix(project_root)
        && !rel.as_os_str().is_empty()
    {
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        return format!("{}/{rel_str}", base_workdir.trim_end_matches('/'));
    }
    base_workdir.to_string()
}

async fn export_task_outputs(
    client: &DaggerClient,
    container_id: &str,
    container_workdir: &str,
    host_workdir: &Path,
    outputs: &[String],
) -> Result<()> {
    for rel_path in outputs {
        if rel_path.is_empty() {
            continue;
        }
        let container_path = if rel_path.starts_with('/') {
            rel_path.clone()
        } else {
            format!("{}/{}", container_workdir.trim_end_matches('/'), rel_path)
        };
        let host_dest = host_workdir.join(rel_path);
        if let Some(parent) = host_dest.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                Error::io_with_path("create output parent directory", parent.to_path_buf(), err)
            })?;
        }
        let host_dest_str = host_dest.to_string_lossy().into_owned();
        client
            .export_path(ExportPathRequest {
                container_id,
                container_path: &container_path,
                host_path: &host_dest_str,
            })
            .await
            .map_err(Error::from)?;
    }
    Ok(())
}

fn render_env_value(value: &EnvValue) -> Option<String> {
    match value {
        EnvValue::String(s) => Some(s.clone()),
        EnvValue::Int(i) => Some(i.to_string()),
        EnvValue::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn render_json_env_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
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
    use cuenv_core::environment::Environment;
    use cuenv_manifest::tasks::TaskDependency;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;

    use crate::session::SessionEndpoint;

    async fn spawn_mock_dagger_server(
        exit_code: i32,
    ) -> (SessionEndpoint, Arc<Mutex<Vec<serde_json::Value>>>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
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
                    recorded_req.lock().await.push(payload);

                    let response_json = if query.contains("host { directory") {
                        serde_json::json!({ "data": { "host": { "directory": { "id": "dir-1" } } } })
                    } else if query.contains("cacheVolume") {
                        serde_json::json!({ "data": { "cacheVolume": { "id": "cache-1" } } })
                    } else if query.contains("setSecret") {
                        serde_json::json!({ "data": { "setSecret": { "id": "secret-1" } } })
                    } else if query.contains("asService") {
                        serde_json::json!({ "data": { "container": { "from": { "asService": { "id": "svc-1" } } } } })
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
                    } else if query.contains("loadContainerFromID") {
                        serde_json::json!({ "data": { "loadContainerFromID": { "file": { "export": true } } } })
                    } else {
                        serde_json::json!({ "errors": [{ "message": format!("unexpected query: {query}") }] })
                    };

                    let resp_body = response_json.to_string();
                    let http = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        resp_body.len(),
                        resp_body
                    );
                    let _ = socket.write_all(http.as_bytes()).await;
                });
            }
        });

        (
            SessionEndpoint {
                port,
                session_token: "test-token".to_string(),
            },
            recorded,
        )
    }

    #[tokio::test]
    async fn executes_dagger_task_with_caches_secrets_services_and_outputs() {
        let (endpoint, recorded) = spawn_mock_dagger_server(0).await;
        let session = Arc::new(DaggerSession::from_endpoint(endpoint));
        let backend = DaggerTaskBackend::new(DaggerBackendConfig::runtime_driven(session));

        let dir = tempfile::tempdir().expect("tempdir");
        let mut task: Task = serde_json::from_value(serde_json::json!({
            "command": "cargo",
            "args": ["test"],
            "inputs": ["src/**"],
            "outputs": ["target/report.txt"],
            "runtime": {
                "type": "dagger",
                "image": "rust:1.88",
                "workdir": "/work",
                "cacheVolumes": [
                    { "key": "cargo-registry", "path": "/usr/local/cargo/registry", "sharing": "SHARED" }
                ]
            }
        }))
        .expect("valid task");
        task.add_typed_dependency(TaskDependency::from_service_name("postgres"));

        let pg: Service = serde_json::from_value(serde_json::json!({
            "type": "service",
            "runtime": { "type": "container", "image": "postgres:16" },
            "ports": [{ "port": 5432 }],
            "env": { "POSTGRES_PASSWORD": "test" }
        }))
        .expect("valid service");
        let mut services = HashMap::new();
        services.insert("postgres".to_string(), pg);

        let mut env = Environment::new();
        env.set("PLAIN_VAR".to_string(), "value".to_string());
        env.set_secret("API_TOKEN".to_string(), "super-secret".to_string());

        let outcome = backend
            .execute_attempt(BackendAttemptRequest {
                name: "integration",
                task: &task,
                environment: &env,
                project_root: dir.path(),
                workdir: dir.path(),
                runtime: task.runtime.as_ref(),
                services: &services,
                timeout: None,
                capture_output: OutputCapture::Capture,
            })
            .await
            .expect("dagger execution should succeed");

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
                && q.contains("expect: ANY")
        }));
        assert!(
            query_strings
                .iter()
                .any(|q| q.contains("loadContainerFromID"))
        );
    }

    #[tokio::test]
    async fn non_zero_exit_code_does_not_export_outputs() {
        let (endpoint, recorded) = spawn_mock_dagger_server(42).await;
        let session = Arc::new(DaggerSession::from_endpoint(endpoint));
        let backend = DaggerTaskBackend::new(DaggerBackendConfig::runtime_driven(session));

        let dir = tempfile::tempdir().expect("tempdir");
        let task: Task = serde_json::from_value(serde_json::json!({
            "script": "exit 42",
            "outputs": ["dist/out.txt"],
            "runtime": { "type": "container", "image": "alpine:3.21" }
        }))
        .expect("valid task");

        let services = HashMap::new();
        let env = Environment::new();
        let outcome = backend
            .execute_attempt(BackendAttemptRequest {
                name: "failing",
                task: &task,
                environment: &env,
                project_root: dir.path(),
                workdir: dir.path(),
                runtime: task.runtime.as_ref(),
                services: &services,
                timeout: None,
                capture_output: OutputCapture::Capture,
            })
            .await
            .expect("non-zero exit should return Completed result");

        let TaskAttempt::Completed(result) = outcome else {
            panic!("expected completed attempt");
        };
        assert!(!result.success);
        assert_eq!(result.exit_code, Some(42));

        let queries = recorded.lock().await;
        assert!(queries.iter().any(|v| {
            v.get("query")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|q| q.contains("withNewFile"))
        }));
        assert!(!queries.iter().any(|v| {
            v.get("query")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|q| q.contains("loadContainerFromID"))
        }));
    }
}
