use crate::RemoteError;
use async_trait::async_trait;
use crate::reapi::build::bazel::remote::execution::v2 as reapi;
use tonic::transport::Channel;
use tonic::Request;

#[async_trait]
pub trait Capabilities {
    async fn get_capabilities(&self) -> Result<reapi::ServerCapabilities, RemoteError>;
}

#[derive(Clone)]
pub struct CapabilitiesClient {
    inner: reapi::capabilities_client::CapabilitiesClient<Channel>,
    instance_name: String,
}

impl CapabilitiesClient {
    pub fn new(channel: Channel, instance_name: String) -> Self {
        Self {
            inner: reapi::capabilities_client::CapabilitiesClient::new(channel),
            instance_name,
        }
    }
}

#[async_trait]
impl Capabilities for CapabilitiesClient {
    async fn get_capabilities(&self) -> Result<reapi::ServerCapabilities, RemoteError> {
        let request = Request::new(reapi::GetCapabilitiesRequest {
            instance_name: self.instance_name.clone(),
        });

        let mut client = self.inner.clone();
        let response = client.get_capabilities(request).await?;
        Ok(response.into_inner())
    }
}
