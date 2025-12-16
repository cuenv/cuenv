//! Terraform provider gRPC client.
//!
//! This module implements the client-side of the tfplugin6 protocol,
//! providing methods for all provider RPC operations.

use std::sync::Arc;
use std::time::Duration;

use tokio::process::Child;
use tokio::sync::RwLock;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tracing::{debug, instrument, warn};

use crate::error::{Error, Result};
use crate::proto::{
    self, check_diagnostics, provider_client::ProviderClient as GrpcClient, ApplyResourceChange,
    ConfigureProvider, DynamicValue, GetProviderSchema, ImportResourceState, PlanResourceChange,
    ReadDataSource, ReadResource, StopProvider, UpgradeResourceState, ValidateProviderConfig,
    ValidateResourceConfig,
};

use super::handshake::{perform_handshake, HandshakeResult};
use super::{ImportedResource, PlannedChange, ProviderSchema, ResourceOperationResult};

/// Client for communicating with a Terraform provider via gRPC.
pub struct ProviderClient {
    /// gRPC client
    client: GrpcClient<Channel>,

    /// Provider process handle
    process: Arc<RwLock<Option<Child>>>,

    /// Handshake result
    handshake: HandshakeResult,

    /// Provider name
    name: String,

    /// Cached schema
    schema: Arc<RwLock<Option<Arc<ProviderSchema>>>>,

    /// Whether the provider has been configured
    configured: Arc<RwLock<bool>>,
}

impl ProviderClient {
    /// Creates a new provider client by spawning the provider process
    /// and performing the handshake.
    ///
    /// # Arguments
    ///
    /// * `name` - Provider name
    /// * `provider_path` - Path to the provider binary
    ///
    /// # Errors
    ///
    /// Returns an error if the provider cannot be started or handshake fails.
    #[instrument(name = "provider_client_new", skip(provider_path))]
    pub async fn new(name: impl Into<String>, provider_path: &str) -> Result<Self> {
        let name = name.into();
        let timeout = Duration::from_secs(30);

        // Perform handshake
        let (process, handshake) = perform_handshake(provider_path, timeout).await?;

        debug!(
            name = %name,
            address = %handshake.address,
            protocol_version = handshake.protocol_version,
            "Provider handshake successful"
        );

        // Build gRPC channel
        let channel = Self::build_channel(&handshake).await?;

        // Create gRPC client
        let client = GrpcClient::new(channel);

        Ok(Self {
            client,
            process: Arc::new(RwLock::new(Some(process))),
            handshake,
            name,
            schema: Arc::new(RwLock::new(None)),
            configured: Arc::new(RwLock::new(false)),
        })
    }

    /// Builds the gRPC channel with optional TLS.
    async fn build_channel(handshake: &HandshakeResult) -> Result<Channel> {
        let endpoint = Endpoint::from_shared(handshake.endpoint_uri())
            .map_err(|e| Error::ProviderHandshakeFailed {
                message: format!("Invalid endpoint URI: {e}"),
            })?;

        let endpoint = if handshake.server_cert.is_some() {
            // Configure TLS
            let tls_config = ClientTlsConfig::new();
            // Note: In production, we would configure the certificate here
            endpoint
                .tls_config(tls_config)
                .map_err(|e| Error::Tls(e.to_string()))?
        } else {
            endpoint
        };

        endpoint
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .connect()
            .await
            .map_err(Error::GrpcTransport)
    }

    /// Returns the provider name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Gets the provider schema, fetching it if not cached.
    ///
    /// # Errors
    ///
    /// Returns an error if the schema cannot be fetched.
    #[instrument(name = "get_schema", skip(self))]
    pub async fn get_schema(&self) -> Result<Arc<ProviderSchema>> {
        // Check cache first
        {
            let cached = self.schema.read().await;
            if let Some(schema) = cached.as_ref() {
                return Ok(Arc::clone(schema));
            }
        }

        // Fetch schema from provider
        let request = GetProviderSchema::Request {};
        let response = self
            .client
            .clone()
            .get_provider_schema(request)
            .await?
            .into_inner();

        check_diagnostics(&response.diagnostics)?;

        let schema = Arc::new(ProviderSchema {
            provider: response.provider,
            resources: response.resource_schemas,
            data_sources: response.data_source_schemas,
        });

        // Cache the schema
        {
            let mut cached = self.schema.write().await;
            *cached = Some(Arc::clone(&schema));
        }

        Ok(schema)
    }

    /// Configures the provider with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - Provider configuration
    ///
    /// # Errors
    ///
    /// Returns an error if configuration fails.
    #[instrument(name = "configure_provider", skip(self, config))]
    pub async fn configure(&self, config: &serde_json::Value) -> Result<()> {
        let request = ConfigureProvider::Request {
            terraform_version: "1.6.0".to_string(),
            config: Some(DynamicValue::from_value(config)?),
            client_capabilities: None,
        };

        let response = self
            .client
            .clone()
            .configure_provider(request)
            .await?
            .into_inner();

        check_diagnostics(&response.diagnostics)?;

        // Mark as configured
        {
            let mut configured = self.configured.write().await;
            *configured = true;
        }

        debug!(provider = %self.name, "Provider configured successfully");

        Ok(())
    }

    /// Validates a provider configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if validation fails.
    #[instrument(name = "validate_provider_config", skip(self, config))]
    pub async fn validate_config(&self, config: &serde_json::Value) -> Result<()> {
        let request = ValidateProviderConfig::Request {
            config: Some(DynamicValue::from_value(config)?),
        };

        let response = self
            .client
            .clone()
            .validate_provider_config(request)
            .await?
            .into_inner();

        check_diagnostics(&response.diagnostics)
    }

    /// Validates a resource configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if validation fails.
    #[instrument(name = "validate_resource_config", skip(self, config))]
    pub async fn validate_resource_config(
        &self,
        type_name: &str,
        config: &serde_json::Value,
    ) -> Result<()> {
        let request = ValidateResourceConfig::Request {
            type_name: type_name.to_string(),
            config: Some(DynamicValue::from_value(config)?),
        };

        let response = self
            .client
            .clone()
            .validate_resource_config(request)
            .await?
            .into_inner();

        check_diagnostics(&response.diagnostics)
    }

    /// Reads the current state of a resource.
    ///
    /// # Arguments
    ///
    /// * `type_name` - Resource type (e.g., "aws_instance")
    /// * `id` - Resource ID (optional, for existing resources)
    ///
    /// # Errors
    ///
    /// Returns an error if reading fails.
    #[instrument(name = "read_resource", skip(self))]
    pub async fn read_resource(
        &self,
        type_name: &str,
        _id: &str,
    ) -> Result<Option<serde_json::Value>> {
        let request = ReadResource::Request {
            type_name: type_name.to_string(),
            current_state: Some(DynamicValue::null()),
            private: Vec::new(),
            provider_meta: None,
            client_capabilities: None,
        };

        let response = self
            .client
            .clone()
            .read_resource(request)
            .await?
            .into_inner();

        check_diagnostics(&response.diagnostics)?;

        match response.new_state {
            Some(state) if !state.is_empty() => Ok(Some(state.to_json_value()?)),
            _ => Ok(None),
        }
    }

    /// Plans a resource change.
    ///
    /// # Arguments
    ///
    /// * `type_name` - Resource type
    /// * `prior_state` - Current state (None for new resources)
    /// * `proposed_state` - Desired state
    /// * `config` - Resource configuration
    ///
    /// # Errors
    ///
    /// Returns an error if planning fails.
    #[instrument(name = "plan_resource_change", skip(self, prior_state, proposed_state, config))]
    pub async fn plan_resource_change(
        &self,
        type_name: &str,
        prior_state: Option<&serde_json::Value>,
        proposed_state: &serde_json::Value,
        config: &serde_json::Value,
    ) -> Result<PlannedChange> {
        let request = PlanResourceChange::Request {
            type_name: type_name.to_string(),
            prior_state: prior_state
                .map(DynamicValue::from_value)
                .transpose()?
                .or_else(|| Some(DynamicValue::null())),
            proposed_new_state: Some(DynamicValue::from_value(proposed_state)?),
            config: Some(DynamicValue::from_value(config)?),
            prior_private: Vec::new(),
            provider_meta: None,
            client_capabilities: None,
        };

        let response = self
            .client
            .clone()
            .plan_resource_change(request)
            .await?
            .into_inner();

        check_diagnostics(&response.diagnostics)?;

        let planned_state = response
            .planned_state
            .map(|s| s.to_json_value())
            .transpose()?
            .unwrap_or(serde_json::Value::Null);

        let requires_replace = response
            .requires_replace
            .iter()
            .filter_map(|p| {
                p.steps
                    .first()
                    .and_then(|s| s.selector.as_ref())
                    .map(|sel| match sel {
                        proto::attribute_path::step::Selector::AttributeName(name) => {
                            name.clone()
                        }
                        proto::attribute_path::step::Selector::ElementKeyInt(i) => {
                            i.to_string()
                        }
                        proto::attribute_path::step::Selector::ElementKeyString(s) => {
                            s.clone()
                        }
                    })
            })
            .collect();

        Ok(PlannedChange {
            planned_state,
            requires_replace,
            planned_private: response.planned_private,
        })
    }

    /// Applies a resource change.
    ///
    /// # Arguments
    ///
    /// * `type_name` - Resource type
    /// * `prior_state` - Current state
    /// * `planned_state` - Planned state from plan_resource_change
    /// * `config` - Resource configuration
    /// * `planned_private` - Private data from plan
    ///
    /// # Errors
    ///
    /// Returns an error if apply fails.
    #[instrument(name = "apply_resource_change", skip(self, prior_state, planned_state, config))]
    pub async fn apply_resource_change(
        &self,
        type_name: &str,
        prior_state: Option<&serde_json::Value>,
        planned_state: &serde_json::Value,
        config: &serde_json::Value,
        planned_private: Vec<u8>,
    ) -> Result<ResourceOperationResult> {
        let request = ApplyResourceChange::Request {
            type_name: type_name.to_string(),
            prior_state: prior_state
                .map(DynamicValue::from_value)
                .transpose()?
                .or_else(|| Some(DynamicValue::null())),
            planned_state: Some(DynamicValue::from_value(planned_state)?),
            config: Some(DynamicValue::from_value(config)?),
            planned_private,
            provider_meta: None,
        };

        let response = self
            .client
            .clone()
            .apply_resource_change(request)
            .await?
            .into_inner();

        check_diagnostics(&response.diagnostics)?;

        let state = response
            .new_state
            .map(|s| s.to_json_value())
            .transpose()?;

        Ok(ResourceOperationResult {
            state,
            private_data: response.private,
            requires_replace: false,
        })
    }

    /// Imports an existing resource.
    ///
    /// # Arguments
    ///
    /// * `type_name` - Resource type
    /// * `id` - Resource ID to import
    ///
    /// # Errors
    ///
    /// Returns an error if import fails.
    #[instrument(name = "import_resource", skip(self))]
    pub async fn import_resource(
        &self,
        type_name: &str,
        id: &str,
    ) -> Result<Vec<ImportedResource>> {
        let request = ImportResourceState::Request {
            type_name: type_name.to_string(),
            id: id.to_string(),
            client_capabilities: None,
        };

        let response = self
            .client
            .clone()
            .import_resource_state(request)
            .await?
            .into_inner();

        check_diagnostics(&response.diagnostics)?;

        let mut resources = Vec::new();
        for imported in response.imported_resources {
            let state = imported
                .state
                .map(|s| s.to_json_value())
                .transpose()?
                .unwrap_or(serde_json::Value::Null);

            resources.push(ImportedResource {
                type_name: imported.type_name,
                state,
                private_data: imported.private,
            });
        }

        Ok(resources)
    }

    /// Reads a data source.
    ///
    /// # Arguments
    ///
    /// * `type_name` - Data source type (e.g., "aws_ami")
    /// * `config` - Data source configuration (filter criteria)
    ///
    /// # Errors
    ///
    /// Returns an error if reading fails.
    #[instrument(name = "read_data_source", skip(self, config))]
    pub async fn read_data_source(
        &self,
        type_name: &str,
        config: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let request = ReadDataSource::Request {
            type_name: type_name.to_string(),
            config: Some(DynamicValue::from_value(config)?),
            provider_meta: None,
            client_capabilities: None,
        };

        let response = self
            .client
            .clone()
            .read_data_source(request)
            .await?
            .into_inner();

        check_diagnostics(&response.diagnostics)?;

        response
            .state
            .map(|s| s.to_json_value())
            .transpose()?
            .ok_or_else(|| Error::ResourceNotFound {
                resource_id: type_name.to_string(),
            })
    }

    /// Upgrades a resource's state from an older schema version.
    ///
    /// # Errors
    ///
    /// Returns an error if upgrade fails.
    #[instrument(name = "upgrade_resource_state", skip(self, raw_state))]
    pub async fn upgrade_resource_state(
        &self,
        type_name: &str,
        version: i64,
        raw_state: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let request = UpgradeResourceState::Request {
            type_name: type_name.to_string(),
            version,
            raw_state: Some(proto::RawState {
                json: serde_json::to_vec(raw_state)?,
                flatmap: std::collections::HashMap::new(),
            }),
        };

        let response = self
            .client
            .clone()
            .upgrade_resource_state(request)
            .await?
            .into_inner();

        check_diagnostics(&response.diagnostics)?;

        response
            .upgraded_state
            .map(|s| s.to_json_value())
            .transpose()?
            .ok_or_else(|| Error::ResourceOperationFailed {
                resource_id: type_name.to_string(),
                message: "Upgrade returned no state".to_string(),
            })
    }

    /// Stops the provider gracefully.
    ///
    /// # Errors
    ///
    /// Returns an error if stopping fails.
    #[instrument(name = "stop_provider", skip(self))]
    pub async fn stop(&self) -> Result<()> {
        let request = StopProvider::Request {};

        let response = self
            .client
            .clone()
            .stop_provider(request)
            .await?
            .into_inner();

        if !response.error.is_empty() {
            warn!(error = %response.error, "Provider stop returned error");
        }

        // Kill the process
        let mut process = self.process.write().await;
        if let Some(mut child) = process.take() {
            let _ = child.kill().await;
        }

        Ok(())
    }

    /// Checks if the provider is still running.
    #[must_use]
    pub async fn is_running(&self) -> bool {
        let process = self.process.read().await;
        process.is_some()
    }
}

impl Drop for ProviderClient {
    fn drop(&mut self) {
        // Attempt to kill the process on drop
        // Note: This is best-effort since we can't await in drop
        if let Ok(mut process) = self.process.try_write() {
            if let Some(mut child) = process.take() {
                let _ = child.start_kill();
            }
        }
    }
}
