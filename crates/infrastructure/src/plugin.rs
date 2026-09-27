//! Launching Terraform provider binaries and speaking their gRPC protocol.
//!
//! Providers are HashiCorp `go-plugin` servers. The host starts the binary
//! with a magic cookie in the environment; the plugin replies with a single
//! handshake line on standard output describing where it listens:
//!
//! ```text
//! CORE-PROTOCOL-VERSION|APP-PROTOCOL-VERSION|NETWORK|ADDRESS|PROTOCOL|SERVER-CERT
//! 1|6|unix|/tmp/cuenv-plugin-1a2b/plugin123|grpc|
//! ```
//!
//! cuenv offers protocol versions 5 and 6 and speaks whichever the provider
//! selects. It does not request AutoMTLS, so providers serve plaintext gRPC
//! on a private unix socket, the same as Terraform with
//! `TF_DISABLE_PLUGIN_TLS`.
//!
//! Each provider gets a private, short socket directory (unix socket paths
//! are limited to about 108 bytes) that is removed when the provider stops,
//! and never inherits the state store's credentials.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hyper_util::rt::TokioIo;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tonic::codegen::http::uri::PathAndQuery;
use tonic::transport::{Channel, Endpoint, Uri};

use crate::error::{InfrastructureError, Result};
use crate::protocol::{self, Diagnostic, DynamicValue};
use crate::schema::ProviderSchema;

/// Magic cookie Terraform providers require before they serve.
const MAGIC_COOKIE_KEY: &str = "TF_PLUGIN_MAGIC_COOKIE";
const MAGIC_COOKIE_VALUE: &str = "d602bf8f470bc67ca7faa0386276bbdd4330efaf76d1a219cb4d6991ca9872b2";

/// Version string cuenv reports to providers during configuration.
/// Providers gate behaviour on this, so report a modern Terraform.
const TERRAFORM_VERSION: &str = "1.9.0";

/// Terraform raises gRPC message limits to 256 MiB; large provider schemas
/// (AWS, Azure) exceed tonic's 4 MiB default.
const MAX_MESSAGE_BYTES: usize = 256 * 1024 * 1024;

/// How long to wait for a provider's handshake line.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);

/// Longest handshake line accepted; real ones are well under 200 bytes.
const MAXIMUM_HANDSHAKE_BYTES: usize = 4096;

/// How long a stopping provider gets for each shutdown step.
const SHUTDOWN_STEP_TIMEOUT: Duration = Duration::from_secs(3);

/// Provider log lines kept for error reports.
const RETAINED_LOG_LINES: usize = 40;

/// go-plugin's controller service, used for a graceful shutdown.
const CONTROLLER_SHUTDOWN_PATH: &str = "/plugin.GRPCController/Shutdown";

/// Plugin protocol major version negotiated with a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// `tfplugin5` — SDKv2 and most multiplexed providers.
    Version5,
    /// `tfplugin6` — Plugin Framework providers.
    Version6,
}

/// Provider RPCs cuenv issues.
#[derive(Debug, Clone, Copy)]
enum RemoteProcedure {
    GetSchema,
    ValidateProviderConfiguration,
    Configure,
    ValidateResourceConfiguration,
    UpgradeResourceState,
    ReadResource,
    PlanResourceChange,
    ApplyResourceChange,
    Stop,
}

impl RemoteProcedure {
    const fn path(self, protocol: Protocol) -> &'static str {
        match (protocol, self) {
            (Protocol::Version5, Self::GetSchema) => "/tfplugin5.Provider/GetSchema",
            (Protocol::Version5, Self::ValidateProviderConfiguration) => {
                "/tfplugin5.Provider/PrepareProviderConfig"
            }
            (Protocol::Version5, Self::Configure) => "/tfplugin5.Provider/Configure",
            (Protocol::Version5, Self::ValidateResourceConfiguration) => {
                "/tfplugin5.Provider/ValidateResourceTypeConfig"
            }
            (Protocol::Version5, Self::UpgradeResourceState) => {
                "/tfplugin5.Provider/UpgradeResourceState"
            }
            (Protocol::Version5, Self::ReadResource) => "/tfplugin5.Provider/ReadResource",
            (Protocol::Version5, Self::PlanResourceChange) => {
                "/tfplugin5.Provider/PlanResourceChange"
            }
            (Protocol::Version5, Self::ApplyResourceChange) => {
                "/tfplugin5.Provider/ApplyResourceChange"
            }
            (Protocol::Version5, Self::Stop) => "/tfplugin5.Provider/Stop",
            (Protocol::Version6, Self::GetSchema) => "/tfplugin6.Provider/GetProviderSchema",
            (Protocol::Version6, Self::ValidateProviderConfiguration) => {
                "/tfplugin6.Provider/ValidateProviderConfig"
            }
            (Protocol::Version6, Self::Configure) => "/tfplugin6.Provider/ConfigureProvider",
            (Protocol::Version6, Self::ValidateResourceConfiguration) => {
                "/tfplugin6.Provider/ValidateResourceConfig"
            }
            (Protocol::Version6, Self::UpgradeResourceState) => {
                "/tfplugin6.Provider/UpgradeResourceState"
            }
            (Protocol::Version6, Self::ReadResource) => "/tfplugin6.Provider/ReadResource",
            (Protocol::Version6, Self::PlanResourceChange) => {
                "/tfplugin6.Provider/PlanResourceChange"
            }
            (Protocol::Version6, Self::ApplyResourceChange) => {
                "/tfplugin6.Provider/ApplyResourceChange"
            }
            (Protocol::Version6, Self::Stop) => "/tfplugin6.Provider/StopProvider",
        }
    }
}

/// Parsed go-plugin handshake line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    /// Negotiated plugin protocol.
    pub protocol: Protocol,
    /// Listener network (`unix` or `tcp`).
    pub network: String,
    /// Listener address.
    pub address: String,
}

impl Handshake {
    /// Parse a go-plugin handshake line.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Plugin`] for malformed lines, unsupported
    /// versions, non-gRPC plugins, or plugins demanding TLS.
    pub fn parse(line: &str) -> Result<Self> {
        let parts: Vec<&str> = line.trim().split('|').collect();
        if parts.len() < 5 {
            return Err(InfrastructureError::plugin(format!(
                "malformed plugin handshake: {line:?}"
            )));
        }
        if parts[0] != "1" {
            return Err(InfrastructureError::plugin(format!(
                "unsupported go-plugin core protocol version {}",
                parts[0]
            )));
        }
        let protocol = match parts[1] {
            "5" => Protocol::Version5,
            "6" => Protocol::Version6,
            other => {
                return Err(InfrastructureError::plugin(format!(
                    "provider selected unsupported plugin protocol {other}"
                )));
            }
        };
        if parts[4] != "grpc" {
            return Err(InfrastructureError::plugin(format!(
                "provider speaks '{}', only grpc is supported",
                parts[4]
            )));
        }
        if parts
            .get(5)
            .is_some_and(|certificate| !certificate.is_empty())
        {
            return Err(InfrastructureError::plugin(
                "provider requested TLS; cuenv only supports plaintext unix-socket plugins",
            ));
        }
        Ok(Self {
            protocol,
            network: parts[2].to_string(),
            address: parts[3].to_string(),
        })
    }
}

/// How to start a provider.
#[derive(Debug, Clone, Copy)]
pub struct LaunchOptions<'launch> {
    /// Provider executable.
    pub binary: &'launch Path,
    /// Environment variables the provider must not inherit, such as the
    /// state store's authentication token.
    pub withheld_environment_variables: &'launch [String],
}

/// Recent provider log lines, attached to errors so failures are diagnosable.
#[derive(Debug, Clone, Default)]
struct ProviderLog {
    lines: Arc<Mutex<VecDeque<String>>>,
}

impl ProviderLog {
    fn record(&self, line: String) {
        if let Ok(mut lines) = self.lines.lock() {
            if lines.len() == RETAINED_LOG_LINES {
                lines.pop_front();
            }
            lines.push_back(line);
        }
    }

    fn render(&self) -> String {
        self.lines
            .lock()
            .map(|lines| {
                if lines.is_empty() {
                    String::new()
                } else {
                    let joined = lines.iter().cloned().collect::<Vec<_>>().join("\n  ");
                    format!("\nrecent provider log:\n  {joined}")
                }
            })
            .unwrap_or_default()
    }
}

/// A private directory for the provider's unix socket, removed on drop.
#[derive(Debug)]
struct SocketDirectory {
    path: PathBuf,
}

impl SocketDirectory {
    fn create() -> Result<Self> {
        let base = if cfg!(unix) && Path::new("/tmp").is_dir() {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let path = base.join(format!("cuenv-plugin-{}", &suffix[..12]));
        std::fs::create_dir(&path).map_err(|error| {
            InfrastructureError::input_output("create provider socket directory", error)
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).map_err(
                |error| {
                    InfrastructureError::input_output("protect provider socket directory", error)
                },
            )?;
        }
        Ok(Self { path })
    }
}

impl Drop for SocketDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A running provider plugin and a gRPC client connected to it.
#[derive(Debug)]
pub struct ProviderClient {
    name: String,
    protocol: Protocol,
    channel: Channel,
    child: Child,
    log: ProviderLog,
    _socket_directory: SocketDirectory,
}

impl ProviderClient {
    /// Start a provider binary and connect to it.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Plugin`] if the binary cannot be started, does
    /// not complete the handshake in time, or cannot be dialed. Errors include
    /// the provider's recent log output.
    #[tracing::instrument(skip_all, fields(binary = %options.binary.display()))]
    pub async fn launch(options: &LaunchOptions<'_>) -> Result<Self> {
        let binary = options.binary;
        let socket_directory = SocketDirectory::create()?;
        let mut command = Command::new(binary);
        for name in options.withheld_environment_variables {
            command.env_remove(name);
        }
        let mut child = command
            .env(MAGIC_COOKIE_KEY, MAGIC_COOKIE_VALUE)
            .env("PLUGIN_PROTOCOL_VERSIONS", "5,6")
            .env("PLUGIN_UNIX_SOCKET_DIR", &socket_directory.path)
            .env("TMPDIR", &socket_directory.path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                InfrastructureError::plugin(format!(
                    "failed to start {}: {error}",
                    binary.display()
                ))
            })?;

        let standard_output = child.stdout.take().ok_or_else(|| {
            InfrastructureError::plugin("provider standard output was not captured")
        })?;
        let standard_error = child.stderr.take().ok_or_else(|| {
            InfrastructureError::plugin("provider standard error was not captured")
        })?;

        let name = binary.file_name().map_or_else(
            || "provider".to_string(),
            |file_name| file_name.to_string_lossy().into_owned(),
        );
        let log = ProviderLog::default();
        spawn_log_drain(name.clone(), standard_error, log.clone());

        let mut reader = BufReader::new(standard_output);
        let line = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_bounded_line(&mut reader))
            .await
            .map_err(|_| {
                InfrastructureError::plugin(format!(
                    "{name} did not complete the plugin handshake within {} seconds{}",
                    HANDSHAKE_TIMEOUT.as_secs(),
                    log.render()
                ))
            })??;
        let Some(line) = line else {
            // Give the log drain a moment to capture why the provider exited.
            tokio::time::sleep(Duration::from_millis(200)).await;
            return Err(InfrastructureError::plugin(format!(
                "{name} exited before the plugin handshake{}",
                log.render()
            )));
        };
        let handshake = Handshake::parse(&line)?;
        tracing::debug!(provider = %name, ?handshake, "provider handshake");

        // Keep draining standard output so the plugin never blocks on a full pipe.
        let mut lines = reader.lines();
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });

        let channel = connect(&handshake)
            .await
            .map_err(|error| InfrastructureError::plugin(format!("{error}{}", log.render())))?;
        Ok(Self {
            name,
            protocol: handshake.protocol,
            channel,
            child,
            log,
            _socket_directory: socket_directory,
        })
    }

    /// Negotiated protocol version.
    #[must_use]
    pub const fn protocol(&self) -> Protocol {
        self.protocol
    }

    async fn call<RequestMessage, ResponseMessage>(
        &self,
        rpc: RemoteProcedure,
        request: RequestMessage,
    ) -> Result<ResponseMessage>
    where
        RequestMessage: prost::Message + Send + Sync + 'static,
        ResponseMessage: prost::Message + Default + Send + Sync + 'static,
    {
        let path = rpc.path(self.protocol);
        let mut grpc = tonic::client::Grpc::new(self.channel.clone())
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        grpc.ready().await.map_err(|error| {
            InfrastructureError::plugin(format!(
                "provider connection not ready for {path}: {error}"
            ))
        })?;
        let codec = tonic_prost::ProstCodec::<RequestMessage, ResponseMessage>::default();
        grpc.unary(
            tonic::Request::new(request),
            PathAndQuery::from_static(path),
            codec,
        )
        .await
        .map(tonic::Response::into_inner)
        .map_err(|status| {
            let transport_failure = matches!(
                status.code(),
                tonic::Code::Unavailable | tonic::Code::Unknown | tonic::Code::Internal
            );
            let status = if transport_failure {
                tonic::Status::new(
                    status.code(),
                    format!("{}{}", status.message(), self.log.render()),
                )
            } else {
                status
            };
            InfrastructureError::RemoteProcedure {
                method: path.to_string(),
                status: Box::new(status),
            }
        })
    }

    /// Fetch the provider and managed resource schemas.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails or the provider reports errors.
    pub async fn schema(&self) -> Result<(ProviderSchema, Vec<Diagnostic>)> {
        match self.protocol {
            Protocol::Version5 => {
                let response: protocol::version5::GetProviderSchemaResponse = self
                    .call(RemoteProcedure::GetSchema, protocol::Empty {})
                    .await?;
                let diagnostics = response.diagnostics.clone();
                Ok((ProviderSchema::from_version5(response)?, diagnostics))
            }
            Protocol::Version6 => {
                let response: protocol::version6::GetProviderSchemaResponse = self
                    .call(RemoteProcedure::GetSchema, protocol::Empty {})
                    .await?;
                let diagnostics = response.diagnostics.clone();
                Ok((ProviderSchema::from_version6(response)?, diagnostics))
            }
        }
    }

    /// Validate provider configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails.
    pub async fn validate_provider_configuration(
        &self,
        configuration: Vec<u8>,
    ) -> Result<Vec<Diagnostic>> {
        let response: protocol::ValidateProviderConfigurationResponse = self
            .call(
                RemoteProcedure::ValidateProviderConfiguration,
                protocol::ValidateProviderConfigurationRequest {
                    configuration: Some(message_pack_value(configuration)),
                },
            )
            .await?;
        Ok(response.diagnostics)
    }

    /// Configure the provider.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails.
    pub async fn configure(&self, configuration: Vec<u8>) -> Result<Vec<Diagnostic>> {
        let response: protocol::DiagnosticsResponse = self
            .call(
                RemoteProcedure::Configure,
                protocol::ConfigureProviderRequest {
                    terraform_version: TERRAFORM_VERSION.to_string(),
                    configuration: Some(message_pack_value(configuration)),
                    client_capabilities: Some(client_capabilities()),
                },
            )
            .await?;
        Ok(response.diagnostics)
    }

    /// Validate a managed resource configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails.
    pub async fn validate_resource_configuration(
        &self,
        type_name: &str,
        configuration: Vec<u8>,
    ) -> Result<Vec<Diagnostic>> {
        let response: protocol::DiagnosticsResponse = self
            .call(
                RemoteProcedure::ValidateResourceConfiguration,
                protocol::ValidateResourceConfigurationRequest {
                    type_name: type_name.to_string(),
                    configuration: Some(message_pack_value(configuration)),
                    client_capabilities: Some(client_capabilities()),
                },
            )
            .await?;
        Ok(response.diagnostics)
    }

    /// Upgrade stored JSON state to the provider's current schema.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails.
    pub async fn upgrade_resource_state(
        &self,
        type_name: &str,
        version: i64,
        state_json: Vec<u8>,
    ) -> Result<(Vec<u8>, Vec<Diagnostic>)> {
        let response: protocol::UpgradeResourceStateResponse = self
            .call(
                RemoteProcedure::UpgradeResourceState,
                protocol::UpgradeResourceStateRequest {
                    type_name: type_name.to_string(),
                    version,
                    raw_state: Some(protocol::RawState {
                        json: state_json,
                        flatmap: std::collections::HashMap::new(),
                    }),
                },
            )
            .await?;
        Ok((
            response
                .upgraded_state
                .map(|dynamic_value| dynamic_value.message_pack)
                .unwrap_or_default(),
            response.diagnostics,
        ))
    }

    /// Refresh a managed resource from the real world.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails.
    pub async fn read_resource(
        &self,
        type_name: &str,
        current_state: Vec<u8>,
        private: Vec<u8>,
    ) -> Result<protocol::ReadResourceResponse> {
        self.call(
            RemoteProcedure::ReadResource,
            protocol::ReadResourceRequest {
                type_name: type_name.to_string(),
                current_state: Some(message_pack_value(current_state)),
                private,
                client_capabilities: Some(client_capabilities()),
            },
        )
        .await
    }

    /// Plan a change to a managed resource.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails.
    pub async fn plan_resource_change(
        &self,
        request: PlanRequest<'_>,
    ) -> Result<protocol::PlanResourceChangeResponse> {
        self.call(
            RemoteProcedure::PlanResourceChange,
            protocol::PlanResourceChangeRequest {
                type_name: request.type_name.to_string(),
                prior_state: Some(message_pack_value(request.prior_state)),
                proposed_new_state: Some(message_pack_value(request.proposed_new_state)),
                configuration: Some(message_pack_value(request.configuration)),
                prior_private: request.prior_private,
                client_capabilities: Some(client_capabilities()),
            },
        )
        .await
    }

    /// Apply a planned change to a managed resource.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails.
    pub async fn apply_resource_change(
        &self,
        request: ApplyRequest<'_>,
    ) -> Result<protocol::ApplyResourceChangeResponse> {
        self.call(
            RemoteProcedure::ApplyResourceChange,
            protocol::ApplyResourceChangeRequest {
                type_name: request.type_name.to_string(),
                prior_state: Some(message_pack_value(request.prior_state)),
                planned_state: Some(message_pack_value(request.planned_state)),
                configuration: Some(message_pack_value(request.configuration)),
                planned_private: request.planned_private,
            },
        )
        .await
    }

    /// Ask the provider to stop gracefully, then terminate the process.
    pub async fn shutdown(mut self) {
        // Ask the provider to cancel in-flight work, then let go-plugin shut
        // down cleanly (it removes its socket), and only then kill it.
        let stop = tokio::time::timeout(
            SHUTDOWN_STEP_TIMEOUT,
            self.call::<_, protocol::StopResponse>(RemoteProcedure::Stop, protocol::Empty {}),
        )
        .await;
        if !matches!(stop, Ok(Ok(_))) {
            tracing::debug!(provider = %self.name, "provider stop procedure did not complete");
        }
        let _ = tokio::time::timeout(SHUTDOWN_STEP_TIMEOUT, self.controller_shutdown()).await;
        if tokio::time::timeout(SHUTDOWN_STEP_TIMEOUT, self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.start_kill();
            let _ = tokio::time::timeout(SHUTDOWN_STEP_TIMEOUT, self.child.wait()).await;
        }
    }

    async fn controller_shutdown(&self) -> Result<()> {
        let mut grpc = tonic::client::Grpc::new(self.channel.clone());
        grpc.ready()
            .await
            .map_err(|error| InfrastructureError::plugin(error.to_string()))?;
        let codec = tonic_prost::ProstCodec::<protocol::Empty, protocol::Empty>::default();
        grpc.unary(
            tonic::Request::new(protocol::Empty {}),
            PathAndQuery::from_static(CONTROLLER_SHUTDOWN_PATH),
            codec,
        )
        .await
        .map(|_| ())
        .map_err(|status| InfrastructureError::plugin(status.to_string()))
    }
}

/// Arguments for [`ProviderClient::plan_resource_change`]. All values are
/// cty MessagePack encoded against the resource schema.
#[derive(Debug)]
pub struct PlanRequest<'request> {
    /// Resource type name.
    pub type_name: &'request str,
    /// Prior state (MessagePack `nil` for create).
    pub prior_state: Vec<u8>,
    /// Proposed new state (MessagePack `nil` for delete).
    pub proposed_new_state: Vec<u8>,
    /// Configuration (MessagePack `nil` for delete).
    pub configuration: Vec<u8>,
    /// Provider private data from prior state.
    pub prior_private: Vec<u8>,
}

/// Arguments for [`ProviderClient::apply_resource_change`].
#[derive(Debug)]
pub struct ApplyRequest<'request> {
    /// Resource type name.
    pub type_name: &'request str,
    /// Prior state.
    pub prior_state: Vec<u8>,
    /// Planned state returned by `PlanResourceChange`.
    pub planned_state: Vec<u8>,
    /// Configuration.
    pub configuration: Vec<u8>,
    /// Private data returned by `PlanResourceChange`.
    pub planned_private: Vec<u8>,
}

const fn message_pack_value(bytes: Vec<u8>) -> DynamicValue {
    DynamicValue {
        message_pack: bytes,
        json: Vec::new(),
    }
}

const fn client_capabilities() -> protocol::ClientCapabilities {
    protocol::ClientCapabilities {
        deferral_allowed: false,
        write_only_attributes_allowed: false,
    }
}

fn spawn_log_drain(name: String, standard_error: tokio::process::ChildStderr, log: ProviderLog) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(standard_error).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if provider_log_is_serious(&line) {
                tracing::warn!(provider = %name, "{line}");
            } else {
                tracing::debug!(provider = %name, "{line}");
            }
            log.record(line);
        }
    });
}

/// go-plugin forwards provider logs as hclog JSON lines with an `@level`.
fn provider_log_is_serious(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|value| {
            value
                .get("@level")
                .and_then(|level| level.as_str().map(str::to_owned))
        })
        .map_or_else(
            || line.contains("panic:") || line.contains("[ERROR]"),
            |level| matches!(level.as_str(), "error" | "warn"),
        )
}

/// Read one line of at most [`MAXIMUM_HANDSHAKE_BYTES`], or `None` at end of
/// output.
async fn read_bounded_line(
    reader: &mut BufReader<tokio::process::ChildStdout>,
) -> Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let buffer = reader
            .fill_buf()
            .await
            .map_err(|error| InfrastructureError::input_output("read provider handshake", error))?;
        if buffer.is_empty() {
            return Ok((!line.is_empty()).then(|| String::from_utf8_lossy(&line).into_owned()));
        }
        let (consumed, finished) = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or((buffer.len(), false), |index| (index + 1, true));
        line.extend_from_slice(&buffer[..consumed]);
        reader.consume(consumed);
        if line.len() > MAXIMUM_HANDSHAKE_BYTES {
            return Err(InfrastructureError::plugin(
                "provider handshake line is too long; is this a Terraform provider?",
            ));
        }
        if finished {
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
    }
}

async fn connect(handshake: &Handshake) -> Result<Channel> {
    match handshake.network.as_str() {
        "unix" => connect_unix(&handshake.address).await,
        "tcp" => Endpoint::from_shared(format!("http://{}", handshake.address))
            .map_err(|error| {
                InfrastructureError::plugin(format!("invalid provider address: {error}"))
            })?
            .connect()
            .await
            .map_err(|error| {
                InfrastructureError::plugin(format!("failed to dial provider: {error}"))
            }),
        other => Err(InfrastructureError::plugin(format!(
            "unsupported provider network '{other}'"
        ))),
    }
}

#[cfg(unix)]
async fn connect_unix(address: &str) -> Result<Channel> {
    let socket = PathBuf::from(address);
    // The URI is ignored by the connector; tonic only needs a valid one.
    Endpoint::from_static("http://[::]:50051")
        .connect_with_connector(tower::service_fn(move |_: Uri| {
            let socket = socket.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(socket).await?;
                Ok::<_, std::io::Error>(TokioIo::new(stream))
            }
        }))
        .await
        .map_err(|error| {
            InfrastructureError::plugin(format!(
                "failed to dial provider socket {address}: {error}"
            ))
        })
}

#[cfg(not(unix))]
async fn connect_unix(address: &str) -> Result<Channel> {
    Err(InfrastructureError::plugin(format!(
        "unix socket providers are not supported on this platform ({address})"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_protocol_5_unix_handshake() {
        let handshake = Handshake::parse("1|5|unix|/tmp/plugin1|grpc|\n").unwrap();
        assert_eq!(handshake.protocol, Protocol::Version5);
        assert_eq!(handshake.network, "unix");
        assert_eq!(handshake.address, "/tmp/plugin1");
    }

    #[test]
    fn parses_protocol_6_handshake_without_trailing_certificate_field() {
        let handshake = Handshake::parse("1|6|tcp|127.0.0.1:1234|grpc").unwrap();
        assert_eq!(handshake.protocol, Protocol::Version6);
        assert_eq!(handshake.network, "tcp");
    }

    #[test]
    fn rejects_netrpc_tls_and_unknown_versions() {
        assert!(Handshake::parse("1|5|unix|/tmp/plugin|netrpc|").is_err());
        assert!(Handshake::parse("1|6|unix|/tmp/plugin|grpc|MIIC...").is_err());
        assert!(Handshake::parse("1|4|unix|/tmp/plugin|grpc|").is_err());
        assert!(Handshake::parse("2|6|unix|/tmp/plugin|grpc|").is_err());
        assert!(Handshake::parse("garbage").is_err());
    }

    #[test]
    fn rpc_paths_follow_protocol_naming() {
        assert_eq!(
            RemoteProcedure::GetSchema.path(Protocol::Version5),
            "/tfplugin5.Provider/GetSchema"
        );
        assert_eq!(
            RemoteProcedure::GetSchema.path(Protocol::Version6),
            "/tfplugin6.Provider/GetProviderSchema"
        );
        assert_eq!(
            RemoteProcedure::ValidateResourceConfiguration.path(Protocol::Version5),
            "/tfplugin5.Provider/ValidateResourceTypeConfig"
        );
        assert_eq!(
            RemoteProcedure::Stop.path(Protocol::Version6),
            "/tfplugin6.Provider/StopProvider"
        );
    }
}
