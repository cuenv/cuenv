//! ActionCache client for caching execution results

use crate::client::channel::{AuthInterceptor, GrpcChannel};
use crate::config::RemoteConfig;
use crate::error::{RemoteError, Result};
use crate::merkle::Digest;
use crate::reapi::{
    ActionResult as ProtoActionResult, GetActionResultRequest, UpdateActionResultRequest,
    action_cache_client::ActionCacheClient as ProtoActionCacheClient,
};
use std::sync::Arc;
use tonic::codegen::InterceptedService;
use tonic::transport::Channel;
use tracing::{debug, instrument};

/// Client for REAPI ActionCache service
pub struct ActionCacheClient {
    client: ProtoActionCacheClient<InterceptedService<Channel, AuthInterceptor>>,
    config: Arc<RemoteConfig>,
}

impl ActionCacheClient {
    /// Create a new ActionCache client from a shared channel
    pub fn from_channel(channel: &GrpcChannel, config: RemoteConfig) -> Self {
        let interceptor = channel.auth_interceptor();
        let client = ProtoActionCacheClient::with_interceptor(channel.channel(), interceptor);
        Self {
            client,
            config: Arc::new(config),
        }
    }

    /// Get a cached action result
    ///
    /// Returns `Ok(Some(result))` on cache hit, `Ok(None)` on cache miss
    #[instrument(skip(self), fields(action_digest = %action_digest))]
    pub async fn get_action_result(
        &self,
        action_digest: &Digest,
    ) -> Result<Option<ProtoActionResult>> {
        let request = GetActionResultRequest {
            instance_name: self.config.instance_name.clone(),
            action_digest: Some(digest_to_proto(action_digest)),
            inline_stdout: true,
            inline_stderr: true,
            inline_output_files: vec![],
            digest_function: 1, // SHA256 (REAPI enum value)
        };

        let mut client = self.client.clone();

        match client.get_action_result(request).await {
            Ok(response) => {
                debug!("Cache hit for action");
                Ok(Some(response.into_inner()))
            }
            Err(status) if status.code() == tonic::Code::NotFound => {
                debug!("Cache miss for action");
                Ok(None)
            }
            Err(status) => Err(RemoteError::grpc_error("GetActionResult", status)),
        }
    }

    /// Update the cached action result
    #[instrument(skip(self, result), fields(action_digest = %action_digest))]
    pub async fn update_action_result(
        &self,
        action_digest: &Digest,
        result: ProtoActionResult,
    ) -> Result<()> {
        let request = UpdateActionResultRequest {
            instance_name: self.config.instance_name.clone(),
            action_digest: Some(digest_to_proto(action_digest)),
            action_result: Some(result),
            results_cache_policy: None,
            digest_function: 1, // SHA256 (REAPI enum value)
        };

        let mut client = self.client.clone();

        client
            .update_action_result(request)
            .await
            .map_err(|e| RemoteError::grpc_error("UpdateActionResult", e))?;

        debug!("Updated action cache");
        Ok(())
    }
}

/// Convert our Digest to proto Digest
fn digest_to_proto(digest: &Digest) -> crate::reapi::Digest {
    crate::reapi::Digest {
        hash: digest.hash.clone(),
        size_bytes: digest.size_bytes,
    }
}
