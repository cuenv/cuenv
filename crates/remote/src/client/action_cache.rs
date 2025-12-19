use crate::merkle::digest::Digest;
use crate::RemoteError;
use async_trait::async_trait;
use crate::reapi::build::bazel::remote::execution::v2 as reapi;
use tonic::transport::Channel;
use tonic::Request;

#[async_trait]
pub trait ActionCache {
    async fn get_action_result(&self, digest: Digest) -> Result<Option<reapi::ActionResult>, RemoteError>;
    async fn update_action_result(&self, digest: Digest, result: reapi::ActionResult) -> Result<reapi::ActionResult, RemoteError>;
}

#[derive(Clone)]
pub struct ActionCacheClient {
    inner: reapi::action_cache_client::ActionCacheClient<Channel>,
    instance_name: String,
}

impl ActionCacheClient {
    pub fn new(channel: Channel, instance_name: String) -> Self {
        Self {
            inner: reapi::action_cache_client::ActionCacheClient::new(channel),
            instance_name,
        }
    }
}

#[async_trait]
impl ActionCache for ActionCacheClient {
    async fn get_action_result(&self, digest: Digest) -> Result<Option<reapi::ActionResult>, RemoteError> {
        let request = Request::new(reapi::GetActionResultRequest {
            instance_name: self.instance_name.clone(),
            action_digest: Some(reapi::Digest {
                hash: digest.hash,
                size_bytes: digest.size_bytes,
            }),
            inline_stdout: true,
            inline_stderr: true,
            inline_output_files: vec![], // TODO: allow configuration
            digest_function: reapi::digest_function::Value::Sha256 as i32,
        });

        let mut client = self.inner.clone();
        match client.get_action_result(request).await {
            Ok(response) => Ok(Some(response.into_inner())),
            Err(status) if status.code() == tonic::Code::NotFound => Ok(None),
            Err(status) => Err(RemoteError::Grpc(status)),
        }
    }

    async fn update_action_result(&self, digest: Digest, result: reapi::ActionResult) -> Result<reapi::ActionResult, RemoteError> {
        let request = Request::new(reapi::UpdateActionResultRequest {
            instance_name: self.instance_name.clone(),
            action_digest: Some(reapi::Digest {
                hash: digest.hash,
                size_bytes: digest.size_bytes,
            }),
            action_result: Some(result),
            results_cache_policy: None, // Use default
            digest_function: reapi::digest_function::Value::Sha256 as i32,
        });

        let mut client = self.inner.clone();
        let response = client.update_action_result(request).await?;
        Ok(response.into_inner())
    }
}
