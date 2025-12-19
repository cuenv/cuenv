use crate::merkle::digest::Digest;
use crate::RemoteError;
use async_trait::async_trait;
use crate::reapi::build::bazel::remote::execution::v2 as reapi;
use crate::reapi::google::longrunning::Operation;
use tonic::transport::Channel;
use tonic::Request;
use futures::stream::{BoxStream, StreamExt};

#[async_trait]
pub trait Execution {
    async fn execute(&self, action_digest: Digest, skip_cache_lookup: bool) -> Result<BoxStream<'static, Result<Operation, RemoteError>>, RemoteError>;
    async fn wait_execution(&self, operation_name: String) -> Result<BoxStream<'static, Result<Operation, RemoteError>>, RemoteError>;
}

#[derive(Clone)]
pub struct ExecutionClient {
    inner: reapi::execution_client::ExecutionClient<Channel>,
    instance_name: String,
}

impl ExecutionClient {
    pub fn new(channel: Channel, instance_name: String) -> Self {
        Self {
            inner: reapi::execution_client::ExecutionClient::new(channel),
            instance_name,
        }
    }
}

#[async_trait]
impl Execution for ExecutionClient {
    async fn execute(&self, action_digest: Digest, skip_cache_lookup: bool) -> Result<BoxStream<'static, Result<Operation, RemoteError>>, RemoteError> {
        let request = Request::new(reapi::ExecuteRequest {
            instance_name: self.instance_name.clone(),
            action_digest: Some(reapi::Digest {
                hash: action_digest.hash,
                size_bytes: action_digest.size_bytes,
            }),
            skip_cache_lookup,
            execution_policy: None,
            results_cache_policy: None,
            digest_function: reapi::digest_function::Value::Sha256 as i32,
            inline_stdout: true,
            inline_stderr: true,
            inline_output_files: vec![], // TODO: allow configuration
        });

        let mut client = self.inner.clone();
        let response = client.execute(request).await?;
        let stream = response.into_inner();

        Ok(Box::pin(stream.map(|res| match res {
            Ok(op) => Ok(op),
            Err(e) => Err(RemoteError::Grpc(e)),
        })))
    }

    async fn wait_execution(&self, operation_name: String) -> Result<BoxStream<'static, Result<Operation, RemoteError>>, RemoteError> {
        let request = Request::new(reapi::WaitExecutionRequest {
            name: operation_name,
        });

        let mut client = self.inner.clone();
        let response = client.wait_execution(request).await?;
        let stream = response.into_inner();

        Ok(Box::pin(stream.map(|res| match res {
            Ok(op) => Ok(op),
            Err(e) => Err(RemoteError::Grpc(e)),
        })))
    }
}
