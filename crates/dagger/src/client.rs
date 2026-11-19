//! Async GraphQL client for the Dagger 1.0 Engine API.
//!
//! Speaks directly to `http://127.0.0.1:<port>/query` using Basic Auth
//! (`base64("<session_token>:")`) over `reqwest`, with zero extra SDK
//! dependencies.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use cuenv_core::manifest::{DaggerCacheSharingMode, DaggerCacheVolume};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::debug;

use crate::error::{DaggerError, Result};
use crate::session::SessionEndpoint;

/// Async GraphQL client bound to a [`SessionEndpoint`].
#[derive(Debug, Clone)]
pub struct DaggerClient {
    endpoint: SessionEndpoint,
    http: reqwest::Client,
    auth_header: reqwest::header::HeaderValue,
}

/// Request to load a host directory into the Dagger Engine.
#[derive(Debug)]
pub struct HostDirectoryRequest<'request> {
    /// Absolute path on the host.
    pub path: &'request str,
    /// Optional include glob patterns (passed directly to `host.directory(include: ...)`).
    pub include: &'request [String],
    /// Optional exclude glob patterns (passed directly to `host.directory(exclude: ...)`).
    pub exclude: &'request [String],
}

/// Container source: either an OCI image reference or a Dockerfile build.
#[derive(Debug)]
pub enum ContainerSource<'source> {
    /// Pull an OCI image (`container.from(address: ...)`).
    Image(&'source str),
    /// Build from a Dockerfile in the mounted workspace (`directory.dockerBuild(dockerfile: ...)`).
    Dockerfile {
        /// Host workspace `DirectoryID` used as Docker build context.
        context_dir_id: &'source str,
        /// Dockerfile path relative to the build context.
        dockerfile: &'source str,
    },
}

/// A mounted Dagger `CacheVolume` bound to a container path.
#[derive(Debug)]
pub struct MountedCacheVolume<'cache> {
    /// Resolved `CacheVolumeID` returned by [`DaggerClient::cache_volume`].
    pub cache_id: &'cache str,
    /// Mount path inside the container.
    pub path: &'cache str,
    /// Cache sharing mode (`SHARED`, `PRIVATE`, `LOCKED`).
    pub sharing: DaggerCacheSharingMode,
}

/// A Dagger `Service` bound to a container hostname alias.
#[derive(Debug)]
pub struct BoundService<'svc> {
    /// Hostname alias reachable from inside the task container.
    pub alias: &'svc str,
    /// Resolved `ServiceID` returned by [`DaggerClient::build_service`].
    pub service_id: &'svc str,
}

/// A secret environment variable backed by a Dagger `SecretID`.
#[derive(Debug)]
pub struct BoundSecret<'secret> {
    /// Environment variable name inside the container.
    pub name: &'secret str,
    /// Resolved `SecretID` returned by [`DaggerClient::set_secret`].
    pub secret_id: &'secret str,
}

/// Controls Dagger-in-Dagger nesting policy on `withExec`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DaggerInDaggerPolicy {
    /// Allow default engine behavior.
    #[default]
    Allow,
    /// Disable nested Dagger-in-Dagger (`disableDaggerInDagger: true`).
    Disable,
}

impl DaggerInDaggerPolicy {
    const fn exec_argument(self) -> &'static str {
        match self {
            Self::Allow => "",
            Self::Disable => ", disableDaggerInDagger: true",
        }
    }
}

/// Request to build a container-backed Dagger `Service` from a `#Service` spec.
#[derive(Debug)]
pub struct ServiceBuildRequest<'request> {
    /// OCI image for the service container.
    pub image: &'request str,
    /// Optional target platform (e.g. `"linux/amd64"`).
    pub platform: Option<&'request str>,
    /// Optional entrypoint/command arguments for `asService(args: ...)`.
    pub args: &'request [String],
    /// Ports to expose via `withExposedPort`.
    pub exposed_ports: &'request [cuenv_core::manifest::ServicePort],
    /// Plain environment variables.
    pub env: &'request BTreeMap<String, String>,
    /// Secret environment variables (`SecretID`s).
    pub secrets: &'request [BoundSecret<'request>],
    /// Services reachable from this service.
    pub services: &'request [BoundService<'request>],
    /// Optional working directory within the service image.
    pub workdir: Option<&'request str>,
    /// Dagger API access policy for the service.
    pub dagger_in_dagger: DaggerInDaggerPolicy,
}

/// Request to execute a command inside a Dagger container.
#[derive(Debug)]
pub struct ContainerExecRequest<'request> {
    /// Container image or Dockerfile build source.
    pub source: ContainerSource<'request>,
    /// Optional target platform (e.g. `"linux/amd64"`).
    pub platform: Option<&'request str>,
    /// Host workspace `DirectoryID` to mount into the container.
    pub workspace_dir_id: &'request str,
    /// Container path where the workspace is mounted.
    pub workspace_path: &'request str,
    /// Container working directory path (e.g. `"/work"`).
    pub workdir: &'request str,
    /// Plain environment variables.
    pub env: &'request BTreeMap<String, String>,
    /// Secret environment variables (`SecretID`s).
    pub secrets: &'request [BoundSecret<'request>],
    /// Mounted Dagger `CacheVolume`s.
    pub caches: &'request [MountedCacheVolume<'request>],
    /// Bound `#Service` containers (`withServiceBinding`).
    pub services: &'request [BoundService<'request>],
    /// Command and arguments passed to `withExec(args: ...)`.
    pub args: &'request [String],
    /// Whether to disable nested Dagger-in-Dagger on `withExec`.
    pub dagger_in_dagger: DaggerInDaggerPolicy,
}

/// Output returned by [`DaggerClient::execute_container`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    /// Resulting `ContainerID` (used to export declared outputs on exit code 0).
    pub container_id: Option<String>,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
    /// Process exit code inside the container.
    pub exit_code: i32,
}

/// Request to export a filtered directory from a completed container.
#[derive(Debug)]
pub struct ExportDirectoryRequest<'request> {
    /// Container ID from a successful execution.
    pub container_id: &'request str,
    /// Directory inside the container.
    pub container_path: &'request str,
    /// Absolute destination path on the host.
    pub host_path: &'request str,
    /// Source prefixes to transfer. An empty list transfers the whole directory.
    pub include: &'request [String],
}

#[derive(Debug, Deserialize)]
struct GraphQlResponse {
    data: Option<Value>,
    #[serde(default)]
    errors: Vec<GraphQlError>,
}

#[derive(Debug, Deserialize)]
struct GraphQlError {
    message: String,
    #[serde(default)]
    extensions: Value,
}

impl GraphQlResponse {
    fn into_data(self) -> Result<Value> {
        if !self.errors.is_empty() {
            let messages: Vec<&str> = self
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect();
            return Err(DaggerError::GraphQl(messages.join("; ")));
        }
        self.data.ok_or_else(|| {
            DaggerError::InvalidResponse(
                "GraphQL response contained neither data nor errors".into(),
            )
        })
    }
}

impl GraphQlError {
    fn command_failure(&self) -> Option<ExecOutput> {
        if self.extensions.get("_type")?.as_str()? != "EXEC_ERROR" {
            return None;
        }
        let exit_code = i32::try_from(self.extensions.get("exitCode")?.as_i64()?).ok()?;
        if exit_code == 0 {
            return None;
        }
        Some(ExecOutput {
            container_id: None,
            stdout: self.extensions.get("stdout")?.as_str()?.to_string(),
            stderr: self.extensions.get("stderr")?.as_str()?.to_string(),
            exit_code,
        })
    }
}

impl DaggerClient {
    /// Build a new [`DaggerClient`] for the given [`SessionEndpoint`].
    ///
    /// # Errors
    ///
    /// Returns [`DaggerError::Transport`] if the HTTP client cannot be constructed.
    pub fn new(endpoint: SessionEndpoint) -> Result<Self> {
        cuenv_core::http::ensure_rustls_crypto_provider()
            .map_err(|err| DaggerError::Session(err.to_string()))?;
        let http = reqwest::Client::builder().no_proxy().build()?;
        let encoded = BASE64_STANDARD.encode(format!("{}:", endpoint.session_token));
        let mut auth_header =
            reqwest::header::HeaderValue::from_str(&format!("Basic {encoded}"))
                .map_err(|_| DaggerError::Session("invalid authorization header".into()))?;
        auth_header.set_sensitive(true);
        Ok(Self {
            endpoint,
            http,
            auth_header,
        })
    }

    /// Execute a raw GraphQL query against the Dagger Engine session.
    ///
    /// # Errors
    ///
    /// Returns [`DaggerError::Transport`] on HTTP failure,
    /// [`DaggerError::GraphQl`] if the engine returns GraphQL errors, or
    /// [`DaggerError::InvalidResponse`] if the response body is missing `data`.
    pub async fn query(&self, query: &str, variables: Value) -> Result<Value> {
        self.query_response(query, variables).await?.into_data()
    }

    async fn query_response(&self, query: &str, variables: Value) -> Result<GraphQlResponse> {
        let response = self
            .http
            .post(self.endpoint.query_url())
            .header(reqwest::header::AUTHORIZATION, &self.auth_header)
            .json(&json!({
                "query": query,
                "variables": variables,
            }))
            .send()
            .await?;

        let status = response.status();
        let body = response.bytes().await?;
        let parsed: GraphQlResponse = serde_json::from_slice(&body).map_err(|error| {
            DaggerError::InvalidResponse(format!(
                "HTTP {status} response from Dagger Engine: {error}"
            ))
        })?;
        if !status.is_success() && parsed.errors.is_empty() {
            return Err(DaggerError::GraphQl(format!(
                "HTTP {status} from Dagger Engine"
            )));
        }
        Ok(parsed)
    }

    /// Load a host directory into the Dagger Engine and return its `DirectoryID`.
    ///
    /// # Errors
    ///
    /// Returns an error if the GraphQL call fails or returns an unexpected shape.
    pub async fn host_directory(&self, request: HostDirectoryRequest<'_>) -> Result<String> {
        let mut var_decls = vec!["$path: String!".to_string()];
        let mut dir_args = vec!["path: $path".to_string()];
        let mut vars = serde_json::Map::new();
        vars.insert("path".to_string(), json!(request.path));

        if !request.include.is_empty() {
            var_decls.push("$include: [String!]".to_string());
            dir_args.push("include: $include".to_string());
            vars.insert("include".to_string(), json!(request.include));
        }
        if !request.exclude.is_empty() {
            var_decls.push("$exclude: [String!]".to_string());
            dir_args.push("exclude: $exclude".to_string());
            vars.insert("exclude".to_string(), json!(request.exclude));
        }

        let query = format!(
            "query({}) {{ host {{ directory({}) {{ id }} }} }}",
            var_decls.join(", "),
            dir_args.join(", ")
        );
        let data = self.query(&query, Value::Object(vars)).await?;
        extract_string(&data, &["host", "directory", "id"])
    }

    /// Create or reference a named Dagger `CacheVolume` and return its `CacheVolumeID`.
    ///
    /// # Errors
    ///
    /// Returns an error if the GraphQL call fails or returns an unexpected shape.
    pub async fn cache_volume(&self, key: &str) -> Result<String> {
        const QUERY: &str = "query($key: String!) { cacheVolume(key: $key) { id } }";
        let data = self.query(QUERY, json!({ "key": key })).await?;
        extract_string(&data, &["cacheVolume", "id"])
    }

    /// Register a plaintext secret with the Dagger Engine and return its `SecretID`.
    ///
    /// # Errors
    ///
    /// Returns an error if the GraphQL call fails or returns an unexpected shape.
    pub async fn set_secret(&self, name: &str, plaintext: &str) -> Result<String> {
        const QUERY: &str = "query($name: String!, $plaintext: String!) { setSecret(name: $name, plaintext: $plaintext) { id } }";
        let data = self
            .query(QUERY, json!({ "name": name, "plaintext": plaintext }))
            .await?;
        extract_string(&data, &["setSecret", "id"])
    }

    /// Build a Dagger `Service` container and return its `ServiceID`.
    ///
    /// # Errors
    ///
    /// Returns an error if the GraphQL call fails or returns an unexpected shape.
    pub async fn build_service(&self, request: ServiceBuildRequest<'_>) -> Result<String> {
        let mut var_decls = vec!["$image: String!".to_string()];
        let mut vars = serde_json::Map::new();
        vars.insert("image".to_string(), json!(request.image));

        let mut chain = String::new();
        let mut close_braces = 0_usize;

        if let Some(platform) = request.platform {
            var_decls.push("$platform: Platform".to_string());
            vars.insert("platform".to_string(), json!(platform));
            chain.push_str("container(platform: $platform) { ");
        } else {
            chain.push_str("container { ");
        }
        close_braces += 1;

        chain.push_str("from(address: $image) { ");
        close_braces += 1;

        if let Some(workdir) = request.workdir {
            var_decls.push("$workdir: String!".to_string());
            vars.insert("workdir".to_string(), json!(workdir));
            chain.push_str("withWorkdir(path: $workdir) { ");
            close_braces += 1;
        }

        for (idx, (k, v)) in request.env.iter().enumerate() {
            let k_var = format!("ek{idx}");
            let v_var = format!("ev{idx}");
            var_decls.push(format!("${k_var}: String!"));
            var_decls.push(format!("${v_var}: String!"));
            vars.insert(k_var.clone(), json!(k));
            vars.insert(v_var.clone(), json!(v));
            let _ = write!(
                chain,
                "withEnvVariable(name: ${k_var}, value: ${v_var}) {{ "
            );
            close_braces += 1;
        }

        for (idx, secret) in request.secrets.iter().enumerate() {
            let n_var = format!("sn{idx}");
            let s_var = format!("sid{idx}");
            var_decls.push(format!("${n_var}: String!"));
            var_decls.push(format!("${s_var}: ID!"));
            vars.insert(n_var.clone(), json!(secret.name));
            vars.insert(s_var.clone(), json!(secret.secret_id));
            let _ = write!(
                chain,
                "withSecretVariable(name: ${n_var}, secret: ${s_var}) {{ "
            );
            close_braces += 1;
        }

        for port in request.exposed_ports {
            let protocol = match port.protocol.to_ascii_lowercase().as_str() {
                "tcp" => "TCP",
                "udp" => "UDP",
                _ => {
                    return Err(DaggerError::Configuration(format!(
                        "unsupported protocol '{}' on service port {}",
                        port.protocol, port.port
                    )));
                }
            };
            let _ = write!(
                chain,
                "withExposedPort(port: {}, protocol: {protocol}) {{ ",
                port.port
            );
            close_braces += 1;
        }

        for (idx, svc) in request.services.iter().enumerate() {
            let a_var = format!("sa{idx}");
            let s_var = format!("svcid{idx}");
            var_decls.push(format!("${a_var}: String!"));
            var_decls.push(format!("${s_var}: ID!"));
            vars.insert(a_var.clone(), json!(svc.alias));
            vars.insert(s_var.clone(), json!(svc.service_id));
            let _ = write!(
                chain,
                "withServiceBinding(alias: ${a_var}, service: ${s_var}) {{ "
            );
            close_braces += 1;
        }

        let dind_arg = request.dagger_in_dagger.exec_argument();
        if request.args.is_empty() {
            let _ = write!(chain, "asService(useEntrypoint: true{dind_arg}) {{ id }}");
        } else {
            var_decls.push("$args: [String!]".to_string());
            vars.insert("args".to_string(), json!(request.args));
            let _ = write!(chain, "asService(args: $args{dind_arg}) {{ id }}");
        }

        for _ in 0..close_braces {
            chain.push_str(" }");
        }

        let query = format!("query({}) {{ {chain} }}", var_decls.join(", "));
        let data = self.query(&query, Value::Object(vars)).await?;
        extract_deepest_field(&data, "id")
    }

    /// Execute a command inside a Dagger container and return its output and exit code.
    ///
    /// Leaves failed executions uncached and reads the engine's structured error.
    ///
    /// # Errors
    ///
    /// Returns an error if the GraphQL call fails or returns an unexpected shape.
    pub async fn execute_container(&self, request: ContainerExecRequest<'_>) -> Result<ExecOutput> {
        let mut var_decls = Vec::new();
        let mut vars = serde_json::Map::new();
        let mut chain = String::new();
        let mut close_braces = 0_usize;

        let platform_arg = if let Some(platform) = request.platform {
            var_decls.push("$platform: Platform".to_string());
            vars.insert("platform".to_string(), json!(platform));
            Some("platform: $platform")
        } else {
            None
        };

        match request.source {
            ContainerSource::Image(image) => {
                var_decls.push("$image: String!".to_string());
                vars.insert("image".to_string(), json!(image));
                if let Some(platform_arg) = platform_arg {
                    let _ = write!(chain, "container({platform_arg}) {{ ");
                } else {
                    chain.push_str("container { ");
                }
                chain.push_str("from(address: $image) { ");
                close_braces += 2;
            }
            ContainerSource::Dockerfile {
                context_dir_id,
                dockerfile,
            } => {
                var_decls.push("$buildCtx: ID!".to_string());
                var_decls.push("$dockerfile: String!".to_string());
                vars.insert("buildCtx".to_string(), json!(context_dir_id));
                vars.insert("dockerfile".to_string(), json!(dockerfile));
                chain.push_str(
                    "node(id: $buildCtx) { ... on Directory { dockerBuild(dockerfile: $dockerfile",
                );
                if let Some(platform_arg) = platform_arg {
                    let _ = write!(chain, ", {platform_arg}");
                }
                chain.push_str(") { ");
                close_braces += 3;
            }
        }

        var_decls.push("$workspacePath: String!".to_string());
        vars.insert("workspacePath".to_string(), json!(request.workspace_path));
        var_decls.push("$workdir: String!".to_string());
        var_decls.push("$workspaceDir: ID!".to_string());
        vars.insert("workdir".to_string(), json!(request.workdir));
        vars.insert("workspaceDir".to_string(), json!(request.workspace_dir_id));
        chain.push_str("withMountedDirectory(path: $workspacePath, source: $workspaceDir) { ");
        close_braces += 1;
        chain.push_str("withWorkdir(path: $workdir) { ");
        close_braces += 1;

        for (idx, cache) in request.caches.iter().enumerate() {
            let p_var = format!("cp{idx}");
            let c_var = format!("cid{idx}");
            var_decls.push(format!("${p_var}: String!"));
            var_decls.push(format!("${c_var}: ID!"));
            vars.insert(p_var.clone(), json!(cache.path));
            vars.insert(c_var.clone(), json!(cache.cache_id));
            let sharing = sharing_mode_graphql(cache.sharing);
            let _ = write!(
                chain,
                "withMountedCache(path: ${p_var}, cache: ${c_var}, sharing: {sharing}) {{ "
            );
            close_braces += 1;
        }

        for (idx, svc) in request.services.iter().enumerate() {
            let a_var = format!("sa{idx}");
            let s_var = format!("svcid{idx}");
            var_decls.push(format!("${a_var}: String!"));
            var_decls.push(format!("${s_var}: ID!"));
            vars.insert(a_var.clone(), json!(svc.alias));
            vars.insert(s_var.clone(), json!(svc.service_id));
            let _ = write!(
                chain,
                "withServiceBinding(alias: ${a_var}, service: ${s_var}) {{ "
            );
            close_braces += 1;
        }

        for (idx, (k, v)) in request.env.iter().enumerate() {
            let k_var = format!("ek{idx}");
            let v_var = format!("ev{idx}");
            var_decls.push(format!("${k_var}: String!"));
            var_decls.push(format!("${v_var}: String!"));
            vars.insert(k_var.clone(), json!(k));
            vars.insert(v_var.clone(), json!(v));
            let _ = write!(
                chain,
                "withEnvVariable(name: ${k_var}, value: ${v_var}) {{ "
            );
            close_braces += 1;
        }

        for (idx, secret) in request.secrets.iter().enumerate() {
            let n_var = format!("sn{idx}");
            let s_var = format!("sid{idx}");
            var_decls.push(format!("${n_var}: String!"));
            var_decls.push(format!("${s_var}: ID!"));
            vars.insert(n_var.clone(), json!(secret.name));
            vars.insert(s_var.clone(), json!(secret.secret_id));
            let _ = write!(
                chain,
                "withSecretVariable(name: ${n_var}, secret: ${s_var}) {{ "
            );
            close_braces += 1;
        }

        var_decls.push("$args: [String!]!".to_string());
        vars.insert("args".to_string(), json!(request.args));
        let dind_arg = request.dagger_in_dagger.exec_argument();
        let _ = write!(
            chain,
            "withExec(args: $args{dind_arg}) {{ id stdout stderr exitCode }}"
        );

        for _ in 0..close_braces {
            chain.push_str(" }");
        }

        let query = format!("query({}) {{ {chain} }}", var_decls.join(", "));
        debug!(query = %query, "executing Dagger container GraphQL query");
        let response = self.query_response(&query, Value::Object(vars)).await?;
        if let Some(failed) = response
            .errors
            .first()
            .and_then(GraphQlError::command_failure)
        {
            return Ok(failed);
        }
        let data = response.into_data()?;

        let exec_node = find_object_with_key(&data, "exitCode").ok_or_else(|| {
            DaggerError::InvalidResponse(
                "missing withExec result object in GraphQL response".to_string(),
            )
        })?;

        let container_id = exec_node
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| DaggerError::InvalidResponse("missing container id".to_string()))?
            .to_string();
        let stdout = exec_node
            .get("stdout")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let stderr = exec_node
            .get("stderr")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let exit_code = exec_node
            .get("exitCode")
            .and_then(Value::as_i64)
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| {
                DaggerError::InvalidResponse("missing or invalid exitCode".to_string())
            })?;

        Ok(ExecOutput {
            container_id: Some(container_id),
            stdout,
            stderr,
            exit_code,
        })
    }

    /// Export a directory from the container to a local staging directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be retrieved or exported.
    pub async fn export_directory(&self, request: ExportDirectoryRequest<'_>) -> Result<()> {
        const QUERY: &str = "query($id: ID!, $src: String!, $dst: String!, $include: [String!]) { \
            node(id: $id) { ... on Container { directory(path: $src, resolveBrokenSymlinks: false) { \
                filter(include: $include) { export(path: $dst) } \
            } } } \
        }";
        let data = self
            .query(
                QUERY,
                json!({
                    "id": request.container_id,
                    "src": request.container_path,
                    "dst": request.host_path,
                    "include": request.include,
                }),
            )
            .await?;
        extract_string(&data, &["node", "directory", "filter", "export"])?;
        Ok(())
    }

    /// Resolve a slice of [`DaggerCacheVolume`] declarations into mounted cache IDs.
    ///
    /// # Errors
    ///
    /// Returns an error if any `cacheVolume` GraphQL call fails.
    pub async fn resolve_cache_volumes(
        &self,
        volumes: &[DaggerCacheVolume],
    ) -> Result<Vec<(String, DaggerCacheVolume)>> {
        let mut resolved = Vec::with_capacity(volumes.len());
        for volume in volumes {
            let id = self.cache_volume(&volume.key).await?;
            resolved.push((id, volume.clone()));
        }
        Ok(resolved)
    }
}

const fn sharing_mode_graphql(mode: DaggerCacheSharingMode) -> &'static str {
    match mode {
        DaggerCacheSharingMode::Shared => "SHARED",
        DaggerCacheSharingMode::Private => "PRIVATE",
        DaggerCacheSharingMode::Locked => "LOCKED",
    }
}

fn extract_string(root: &Value, path: &[&str]) -> Result<String> {
    let mut current = root;
    for key in path {
        current = current.get(*key).ok_or_else(|| {
            DaggerError::InvalidResponse(format!(
                "missing field '{}' in GraphQL response",
                path.join(".")
            ))
        })?;
    }
    current.as_str().map(str::to_string).ok_or_else(|| {
        DaggerError::InvalidResponse(format!(
            "expected string at '{}' in GraphQL response",
            path.join(".")
        ))
    })
}

fn extract_deepest_field(root: &Value, key: &str) -> Result<String> {
    find_object_with_key(root, key)
        .and_then(|obj| obj.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            DaggerError::InvalidResponse(format!(
                "missing string field '{key}' in GraphQL response"
            ))
        })
}

fn find_object_with_key<'v>(
    value: &'v Value,
    key: &str,
) -> Option<&'v serde_json::Map<String, Value>> {
    match value {
        Value::Object(map) => {
            if map.contains_key(key) {
                return Some(map);
            }
            for child in map.values() {
                if let Some(found) = find_object_with_key(child, key) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items
            .iter()
            .find_map(|item| find_object_with_key(item, key)),
        _ => None,
    }
}
