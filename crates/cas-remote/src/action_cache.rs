//! [`ActionCache`] over the REAPI `ActionCache` service.

use crate::client::RemoteClient;
use crate::error::{Error, Result};
use async_trait::async_trait;
use bazel_remote_apis::build::bazel::remote::execution::v2 as pb;
use bazel_remote_apis::build::bazel::remote::execution::v2::action_cache_client::ActionCacheClient;
use cuenv_cas::{ActionCache, ActionResult, CanonicalMessage, Digest};
use tonic::Code;
use tracing::trace;

/// An action cache backed by a REAPI server.
#[derive(Debug)]
pub struct RemoteActionCache {
    client: RemoteClient,
}

impl RemoteActionCache {
    /// Build an action cache on an existing client.
    #[must_use]
    pub fn new(client: RemoteClient) -> Self {
        Self { client }
    }

    fn action_cache_client(&self) -> ActionCacheClient<tonic::transport::Channel> {
        ActionCacheClient::new(self.client.channel())
    }

    async fn get(&self, action_digest: &Digest) -> Result<Option<ActionResult>> {
        let request = self.client.request(pb::GetActionResultRequest {
            instance_name: self.client.instance_name(),
            action_digest: Some(action_digest.to_proto()?),
            // cuenv fetches stdout and stderr from the CAS alongside the
            // output files, so it does not ask the server to inline them.
            inline_stdout: false,
            inline_stderr: false,
            ..Default::default()
        })?;

        match self.action_cache_client().get_action_result(request).await {
            Ok(response) => {
                trace!(action = %action_digest, "remote action cache hit");
                Ok(Some(ActionResult::from_proto(&response.into_inner())?))
            }
            // A miss is the expected outcome most of the time, and REAPI
            // spells it `NOT_FOUND`. Treating it as an error would make every
            // cold lookup a failure.
            Err(status) if status.code() == Code::NotFound => {
                trace!(action = %action_digest, "remote action cache miss");
                Ok(None)
            }
            Err(status) => Err(Error::rpc("GetActionResult", &status)),
        }
    }

    async fn put(&self, action_digest: &Digest, result: &ActionResult) -> Result<()> {
        self.client.ensure_writable("update an action result")?;

        let request = self.client.request(pb::UpdateActionResultRequest {
            instance_name: self.client.instance_name(),
            action_digest: Some(action_digest.to_proto()?),
            action_result: Some(result.to_proto()?),
            ..Default::default()
        })?;

        self.action_cache_client()
            .update_action_result(request)
            .await
            .map_err(|status| Error::rpc("UpdateActionResult", &status))?;
        trace!(action = %action_digest, "remote action cache updated");
        Ok(())
    }
}

#[async_trait]
impl ActionCache for RemoteActionCache {
    async fn lookup(&self, action_digest: &Digest) -> cuenv_cas::Result<Option<ActionResult>> {
        Ok(self.get(action_digest).await?)
    }

    async fn update(
        &self,
        action_digest: &Digest,
        result: &ActionResult,
    ) -> cuenv_cas::Result<()> {
        Ok(self.put(action_digest, result).await?)
    }
}
