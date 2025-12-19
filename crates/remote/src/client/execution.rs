//! Execution client for remote task execution

use crate::client::channel::{AuthInterceptor, GrpcChannel};
use crate::config::RemoteConfig;
use crate::error::{RemoteError, Result};
use crate::merkle::Digest;
use crate::proto::google::longrunning::operation::Result as OperationResult;
use crate::reapi::{
    ActionResult as ProtoActionResult, ExecuteOperationMetadata, ExecuteRequest, ExecuteResponse,
    WaitExecutionRequest, execution_client::ExecutionClient as ProtoExecutionClient,
};
use prost::Message;
use std::sync::Arc;
use tonic::codegen::InterceptedService;
use tonic::transport::Channel;
use tracing::{debug, info, instrument, warn};

/// Client for REAPI Execution service
pub struct ExecutionClient {
    client: ProtoExecutionClient<InterceptedService<Channel, AuthInterceptor>>,
    config: Arc<RemoteConfig>,
}

impl ExecutionClient {
    /// Create a new Execution client from a shared channel
    pub fn from_channel(channel: &GrpcChannel, config: RemoteConfig) -> Self {
        let interceptor = channel.auth_interceptor();
        let client = ProtoExecutionClient::with_interceptor(channel.channel(), interceptor);
        Self {
            client,
            config: Arc::new(config),
        }
    }

    /// Execute an action remotely
    ///
    /// Streams Operation updates and waits for completion
    #[instrument(skip(self), fields(action_digest = %action_digest))]
    pub async fn execute(&self, action_digest: &Digest) -> Result<ProtoActionResult> {
        let request = ExecuteRequest {
            instance_name: self.config.instance_name.clone(),
            action_digest: Some(digest_to_proto(action_digest)),
            skip_cache_lookup: false,
            execution_policy: None,
            results_cache_policy: None,
            digest_function: 1, // SHA256 (REAPI enum value)
            inline_stdout: true,
            inline_stderr: true,
            inline_output_files: vec![],
        };

        let mut client = self.client.clone();

        info!("Submitting execution request");
        let mut stream = client
            .execute(request)
            .await
            .map_err(|e| RemoteError::grpc_error("Execute", e))?
            .into_inner();

        // Process streaming operations
        let mut final_result: Option<ProtoActionResult> = None;
        let mut operation_name: Option<String> = None;

        while let Some(operation) = stream
            .message()
            .await
            .map_err(|e| RemoteError::grpc_error("Execute stream", e))?
        {
            operation_name = Some(operation.name.clone());

            // Log execution progress from metadata
            if let Some(ref metadata_any) = operation.metadata {
                if let Ok(metadata) = ExecuteOperationMetadata::decode(metadata_any.value.as_ref())
                {
                    log_execution_stage(metadata.stage);
                }
            }

            if operation.done {
                // Extract result from completed operation
                match operation.result {
                    Some(OperationResult::Response(response_any)) => {
                        let response = ExecuteResponse::decode(response_any.value.as_ref())
                            .map_err(|e| {
                                RemoteError::serialization_error(format!(
                                    "Failed to decode ExecuteResponse: {}",
                                    e
                                ))
                            })?;

                        // Check for server-side error status
                        if let Some(status) = response.status {
                            if status.code != 0 {
                                warn!(
                                    code = status.code,
                                    message = %status.message,
                                    "Execution returned error status"
                                );
                            }
                        }

                        if let Some(result) = response.result {
                            final_result = Some(result);
                        } else {
                            return Err(RemoteError::execution_failed(
                                "ExecuteResponse missing result",
                            ));
                        }
                    }
                    Some(OperationResult::Error(status)) => {
                        return Err(RemoteError::execution_failed(format!(
                            "Execution failed: {} (code {})",
                            status.message, status.code
                        )));
                    }
                    None => {
                        return Err(RemoteError::execution_failed(
                            "Operation completed without result",
                        ));
                    }
                }
                break;
            }
        }

        final_result.ok_or_else(|| {
            RemoteError::execution_failed(format!(
                "Stream ended without completion (operation: {})",
                operation_name.unwrap_or_else(|| "unknown".to_string())
            ))
        })
    }

    /// Wait for an operation to complete
    #[instrument(skip(self))]
    pub async fn wait_execution(&self, operation_name: &str) -> Result<ProtoActionResult> {
        let request = WaitExecutionRequest {
            name: operation_name.to_string(),
        };

        let mut client = self.client.clone();

        let mut stream = client
            .wait_execution(request)
            .await
            .map_err(|e| RemoteError::grpc_error("WaitExecution", e))?
            .into_inner();

        let mut final_result: Option<ProtoActionResult> = None;

        while let Some(operation) = stream
            .message()
            .await
            .map_err(|e| RemoteError::grpc_error("WaitExecution stream", e))?
        {
            // Log execution progress
            if let Some(ref metadata_any) = operation.metadata {
                if let Ok(metadata) = ExecuteOperationMetadata::decode(metadata_any.value.as_ref())
                {
                    log_execution_stage(metadata.stage);
                }
            }

            if operation.done {
                match operation.result {
                    Some(OperationResult::Response(response_any)) => {
                        let response = ExecuteResponse::decode(response_any.value.as_ref())
                            .map_err(|e| {
                                RemoteError::serialization_error(format!(
                                    "Failed to decode ExecuteResponse: {}",
                                    e
                                ))
                            })?;

                        if let Some(result) = response.result {
                            final_result = Some(result);
                        }
                    }
                    Some(OperationResult::Error(status)) => {
                        return Err(RemoteError::execution_failed(format!(
                            "Execution failed: {} (code {})",
                            status.message, status.code
                        )));
                    }
                    None => {}
                }
                break;
            }
        }

        final_result
            .ok_or_else(|| RemoteError::execution_failed("Wait stream ended without completion"))
    }
}

/// Log execution stage for progress tracking
/// Stage values per REAPI spec: UNKNOWN=0, CACHE_CHECK=1, QUEUED=2, EXECUTING=3, COMPLETED=4
fn log_execution_stage(stage: i32) {
    let stage_name = match stage {
        0 => "unknown",
        1 => "cache_check",
        2 => "queued",
        3 => "executing",
        4 => "completed",
        _ => "unknown",
    };
    debug!(stage = stage_name, "Execution progress");
}

/// Convert our Digest to proto Digest
fn digest_to_proto(digest: &Digest) -> crate::reapi::Digest {
    crate::reapi::Digest {
        hash: digest.hash.clone(),
        size_bytes: digest.size_bytes,
    }
}
