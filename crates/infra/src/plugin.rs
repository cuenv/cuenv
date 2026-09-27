//! Launching Terraform provider binaries and speaking their gRPC protocol.
//!
//! Providers are HashiCorp `go-plugin` servers. The host starts the binary
//! with a magic cookie in the environment; the plugin replies with a single
//! handshake line on stdout describing where it listens:
//!
//! ```text
//! CORE-PROTOCOL-VERSION|APP-PROTOCOL-VERSION|NETWORK|ADDRESS|PROTOCOL|SERVER-CERT
//! 1|6|unix|/tmp/plugin123|grpc|
//! ```
//!
//! cuenv offers protocol versions 5 and 6 and speaks whichever the provider
//! selects. It does not request AutoMTLS, so providers serve plaintext gRPC
//! on a private unix socket, the same as Terraform with
//! `TF_DISABLE_PLUGIN_TLS`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use hyper_util::rt::TokioIo;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tonic::codegen::http::uri::PathAndQuery;
use tonic::transport::{Channel, Endpoint, Uri};

use crate::error::{InfraError, Result};
use crate::proto::{self, Diagnostic, DynamicValue};
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

/// Plugin protocol major version negotiated with a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// `tfplugin5` — SDKv2 and most mux'd providers.
    V5,
    /// `tfplugin6` — Plugin Framework providers.
    V6,
}

/// Provider RPCs cuenv issues.
#[derive(Debug, Clone, Copy)]
enum Rpc {
    GetSchema,
    ValidateProviderConfig,
    Configure,
    ValidateResourceConfig,
    UpgradeResourceState,
    ReadResource,
    PlanResourceChange,
    ApplyResourceChange,
    Stop,
}

impl Rpc {
    const fn path(self, protocol: Protocol) -> &'static str {
        match (protocol, self) {
            (Protocol::V5, Self::GetSchema) => "/tfplugin5.Provider/GetSchema",
            (Protocol::V5, Self::ValidateProviderConfig) => {
                "/tfplugin5.Provider/PrepareProviderConfig"
            }
            (Protocol::V5, Self::Configure) => "/tfplugin5.Provider/Configure",
            (Protocol::V5, Self::ValidateResourceConfig) => {
                "/tfplugin5.Provider/ValidateResourceTypeConfig"
            }
            (Protocol::V5, Self::UpgradeResourceState) => {
                "/tfplugin5.Provider/UpgradeResourceState"
            }
            (Protocol::V5, Self::ReadResource) => "/tfplugin5.Provider/ReadResource",
            (Protocol::V5, Self::PlanResourceChange) => "/tfplugin5.Provider/PlanResourceChange",
            (Protocol::V5, Self::ApplyResourceChange) => "/tfplugin5.Provider/ApplyResourceChange",
            (Protocol::V5, Self::Stop) => "/tfplugin5.Provider/Stop",
            (Protocol::V6, Self::GetSchema) => "/tfplugin6.Provider/GetProviderSchema",
            (Protocol::V6, Self::ValidateProviderConfig) => {
                "/tfplugin6.Provider/ValidateProviderConfig"
            }
            (Protocol::V6, Self::Configure) => "/tfplugin6.Provider/ConfigureProvider",
            (Protocol::V6, Self::ValidateResourceConfig) => {
                "/tfplugin6.Provider/ValidateResourceConfig"
            }
            (Protocol::V6, Self::UpgradeResourceState) => {
                "/tfplugin6.Provider/UpgradeResourceState"
            }
            (Protocol::V6, Self::ReadResource) => "/tfplugin6.Provider/ReadResource",
            (Protocol::V6, Self::PlanResourceChange) => "/tfplugin6.Provider/PlanResourceChange",
            (Protocol::V6, Self::ApplyResourceChange) => "/tfplugin6.Provider/ApplyResourceChange",
            (Protocol::V6, Self::Stop) => "/tfplugin6.Provider/StopProvider",
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
    /// Returns [`InfraError::Plugin`] for malformed lines, unsupported
    /// versions, non-gRPC plugins, or plugins demanding TLS.
    pub fn parse(line: &str) -> Result<Self> {
        let parts: Vec<&str> = line.trim().split('|').collect();
        if parts.len() < 5 {
            return Err(InfraError::plugin(format!(
                "malformed plugin handshake: {line:?}"
            )));
        }
        if parts[0] != "1" {
            return Err(InfraError::plugin(format!(
                "unsupported go-plugin core protocol version {}",
                parts[0]
            )));
        }
        let protocol = match parts[1] {
            "5" => Protocol::V5,
            "6" => Protocol::V6,
            other => {
                return Err(InfraError::plugin(format!(
                    "provider selected unsupported plugin protocol {other}"
                )));
            }
        };
        if parts[4] != "grpc" {
            return Err(InfraError::plugin(format!(
                "provider speaks '{}', only grpc is supported",
                parts[4]
            )));
        }
        if parts.get(5).is_some_and(|cert| !cert.is_empty()) {
            return Err(InfraError::plugin(
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

/// A running provider plugin and a gRPC client connected to it.
#[derive(Debug)]
pub struct ProviderClient {
    binary: PathBuf,
    protocol: Protocol,
    channel: Channel,
    child: Child,
}

impl ProviderClient {
    /// Start a provider binary and connect to it.
    ///
    /// # Errors
    ///
    /// Returns [`InfraError::Plugin`] if the binary cannot be started, does
    /// not complete the handshake in time, or cannot be dialed.
    pub async fn launch(binary: &Path) -> Result<Self> {
        let mut child = Command::new(binary)
            .env(MAGIC_COOKIE_KEY, MAGIC_COOKIE_VALUE)
            .env("PLUGIN_PROTOCOL_VERSIONS", "5,6")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                InfraError::plugin(format!("failed to start {}: {e}", binary.display()))
            })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| InfraError::plugin("provider stdout was not captured"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| InfraError::plugin("provider stderr was not captured"))?;

        let name = binary.file_name().map_or_else(
            || "provider".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        spawn_log_drain(name.clone(), stderr);

        let mut lines = BufReader::new(stdout).lines();
        let line = tokio::time::timeout(HANDSHAKE_TIMEOUT, lines.next_line())
            .await
            .map_err(|_| {
                InfraError::plugin(format!("{name} did not complete the plugin handshake"))
            })?
            .map_err(|e| InfraError::io("read provider handshake", e))?
            .ok_or_else(|| {
                InfraError::plugin(format!("{name} exited before the plugin handshake"))
            })?;
        let handshake = Handshake::parse(&line)?;
        tracing::debug!(provider = %name, ?handshake, "provider handshake");

        // Keep draining stdout so the plugin never blocks on a full pipe.
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });

        let channel = connect(&handshake).await?;
        Ok(Self {
            binary: binary.to_path_buf(),
            protocol: handshake.protocol,
            channel,
            child,
        })
    }

    /// Negotiated protocol version.
    #[must_use]
    pub const fn protocol(&self) -> Protocol {
        self.protocol
    }

    /// Path of the running provider binary.
    #[must_use]
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    async fn call<Req, Resp>(&self, rpc: Rpc, request: Req) -> Result<Resp>
    where
        Req: prost::Message + Send + Sync + 'static,
        Resp: prost::Message + Default + Send + Sync + 'static,
    {
        let path = rpc.path(self.protocol);
        let mut grpc = tonic::client::Grpc::new(self.channel.clone())
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        grpc.ready().await.map_err(|e| {
            InfraError::plugin(format!("provider connection not ready for {path}: {e}"))
        })?;
        let codec = tonic_prost::ProstCodec::<Req, Resp>::default();
        grpc.unary(
            tonic::Request::new(request),
            PathAndQuery::from_static(path),
            codec,
        )
        .await
        .map(tonic::Response::into_inner)
        .map_err(|status| InfraError::Rpc {
            method: path.to_string(),
            status: Box::new(status),
        })
    }

    /// Fetch the provider and managed resource schemas.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails or the provider reports errors.
    pub async fn schema(&self) -> Result<(ProviderSchema, Vec<Diagnostic>)> {
        match self.protocol {
            Protocol::V5 => {
                let resp: proto::v5::GetProviderSchemaResponse =
                    self.call(Rpc::GetSchema, proto::Empty {}).await?;
                let diags = resp.diagnostics.clone();
                Ok((ProviderSchema::from_v5(resp)?, diags))
            }
            Protocol::V6 => {
                let resp: proto::v6::GetProviderSchemaResponse =
                    self.call(Rpc::GetSchema, proto::Empty {}).await?;
                let diags = resp.diagnostics.clone();
                Ok((ProviderSchema::from_v6(resp)?, diags))
            }
        }
    }

    /// Validate provider configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails.
    pub async fn validate_provider_config(&self, config: Vec<u8>) -> Result<Vec<Diagnostic>> {
        let resp: proto::ValidateProviderConfigResponse = self
            .call(
                Rpc::ValidateProviderConfig,
                proto::ValidateProviderConfigRequest {
                    config: Some(msgpack(config)),
                },
            )
            .await?;
        Ok(resp.diagnostics)
    }

    /// Configure the provider.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails.
    pub async fn configure(&self, config: Vec<u8>) -> Result<Vec<Diagnostic>> {
        let resp: proto::DiagnosticsResponse = self
            .call(
                Rpc::Configure,
                proto::ConfigureProviderRequest {
                    terraform_version: TERRAFORM_VERSION.to_string(),
                    config: Some(msgpack(config)),
                    client_capabilities: Some(client_capabilities()),
                },
            )
            .await?;
        Ok(resp.diagnostics)
    }

    /// Validate a managed resource configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails.
    pub async fn validate_resource_config(
        &self,
        type_name: &str,
        config: Vec<u8>,
    ) -> Result<Vec<Diagnostic>> {
        let resp: proto::DiagnosticsResponse = self
            .call(
                Rpc::ValidateResourceConfig,
                proto::ValidateResourceConfigRequest {
                    type_name: type_name.to_string(),
                    config: Some(msgpack(config)),
                    client_capabilities: Some(client_capabilities()),
                },
            )
            .await?;
        Ok(resp.diagnostics)
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
        let resp: proto::UpgradeResourceStateResponse = self
            .call(
                Rpc::UpgradeResourceState,
                proto::UpgradeResourceStateRequest {
                    type_name: type_name.to_string(),
                    version,
                    raw_state: Some(proto::RawState {
                        json: state_json,
                        flatmap: std::collections::HashMap::new(),
                    }),
                },
            )
            .await?;
        Ok((
            resp.upgraded_state.map(|v| v.msgpack).unwrap_or_default(),
            resp.diagnostics,
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
    ) -> Result<proto::ReadResourceResponse> {
        self.call(
            Rpc::ReadResource,
            proto::ReadResourceRequest {
                type_name: type_name.to_string(),
                current_state: Some(msgpack(current_state)),
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
    ) -> Result<proto::PlanResourceChangeResponse> {
        self.call(
            Rpc::PlanResourceChange,
            proto::PlanResourceChangeRequest {
                type_name: request.type_name.to_string(),
                prior_state: Some(msgpack(request.prior_state)),
                proposed_new_state: Some(msgpack(request.proposed_new_state)),
                config: Some(msgpack(request.config)),
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
    ) -> Result<proto::ApplyResourceChangeResponse> {
        self.call(
            Rpc::ApplyResourceChange,
            proto::ApplyResourceChangeRequest {
                type_name: request.type_name.to_string(),
                prior_state: Some(msgpack(request.prior_state)),
                planned_state: Some(msgpack(request.planned_state)),
                config: Some(msgpack(request.config)),
                planned_private: request.planned_private,
            },
        )
        .await
    }

    /// Ask the provider to stop gracefully, then terminate the process.
    pub async fn shutdown(mut self) {
        let stop: Result<proto::StopResponse> = self.call(Rpc::Stop, proto::Empty {}).await;
        if let Err(e) = stop {
            tracing::debug!(error = %e, "provider stop RPC failed");
        }
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
    }
}

/// Arguments for [`ProviderClient::plan_resource_change`]. All values are
/// cty msgpack encoded against the resource schema.
#[derive(Debug)]
pub struct PlanRequest<'a> {
    /// Resource type name.
    pub type_name: &'a str,
    /// Prior state (msgpack `nil` for create).
    pub prior_state: Vec<u8>,
    /// Proposed new state (msgpack `nil` for delete).
    pub proposed_new_state: Vec<u8>,
    /// Configuration (msgpack `nil` for delete).
    pub config: Vec<u8>,
    /// Provider private data from prior state.
    pub prior_private: Vec<u8>,
}

/// Arguments for [`ProviderClient::apply_resource_change`].
#[derive(Debug)]
pub struct ApplyRequest<'a> {
    /// Resource type name.
    pub type_name: &'a str,
    /// Prior state.
    pub prior_state: Vec<u8>,
    /// Planned state returned by `PlanResourceChange`.
    pub planned_state: Vec<u8>,
    /// Configuration.
    pub config: Vec<u8>,
    /// Private data returned by `PlanResourceChange`.
    pub planned_private: Vec<u8>,
}

const fn msgpack(bytes: Vec<u8>) -> DynamicValue {
    DynamicValue {
        msgpack: bytes,
        json: Vec::new(),
    }
}

const fn client_capabilities() -> proto::ClientCapabilities {
    proto::ClientCapabilities {
        deferral_allowed: false,
        write_only_attributes_allowed: false,
    }
}

fn spawn_log_drain(name: String, stderr: tokio::process::ChildStderr) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(provider = %name, "{line}");
        }
    });
}

async fn connect(handshake: &Handshake) -> Result<Channel> {
    match handshake.network.as_str() {
        "unix" => connect_unix(&handshake.address).await,
        "tcp" => Endpoint::from_shared(format!("http://{}", handshake.address))
            .map_err(|e| InfraError::plugin(format!("invalid provider address: {e}")))?
            .connect()
            .await
            .map_err(|e| InfraError::plugin(format!("failed to dial provider: {e}"))),
        other => Err(InfraError::plugin(format!(
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
        .map_err(|e| InfraError::plugin(format!("failed to dial provider socket {address}: {e}")))
}

#[cfg(not(unix))]
async fn connect_unix(address: &str) -> Result<Channel> {
    Err(InfraError::plugin(format!(
        "unix socket providers are not supported on this platform ({address})"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_protocol_5_unix_handshake() {
        let hs = Handshake::parse("1|5|unix|/tmp/plugin1|grpc|\n").unwrap();
        assert_eq!(hs.protocol, Protocol::V5);
        assert_eq!(hs.network, "unix");
        assert_eq!(hs.address, "/tmp/plugin1");
    }

    #[test]
    fn parses_protocol_6_handshake_without_trailing_cert_field() {
        let hs = Handshake::parse("1|6|tcp|127.0.0.1:1234|grpc").unwrap();
        assert_eq!(hs.protocol, Protocol::V6);
        assert_eq!(hs.network, "tcp");
    }

    #[test]
    fn rejects_netrpc_tls_and_unknown_versions() {
        assert!(Handshake::parse("1|5|unix|/tmp/p|netrpc|").is_err());
        assert!(Handshake::parse("1|6|unix|/tmp/p|grpc|MIIC...").is_err());
        assert!(Handshake::parse("1|4|unix|/tmp/p|grpc|").is_err());
        assert!(Handshake::parse("2|6|unix|/tmp/p|grpc|").is_err());
        assert!(Handshake::parse("garbage").is_err());
    }

    #[test]
    fn rpc_paths_follow_protocol_naming() {
        assert_eq!(
            Rpc::GetSchema.path(Protocol::V5),
            "/tfplugin5.Provider/GetSchema"
        );
        assert_eq!(
            Rpc::GetSchema.path(Protocol::V6),
            "/tfplugin6.Provider/GetProviderSchema"
        );
        assert_eq!(
            Rpc::ValidateResourceConfig.path(Protocol::V5),
            "/tfplugin5.Provider/ValidateResourceTypeConfig"
        );
        assert_eq!(
            Rpc::Stop.path(Protocol::V6),
            "/tfplugin6.Provider/StopProvider"
        );
    }
}
