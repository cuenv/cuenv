//! [`ActionCache`] over the Bazel HTTP/REST remote cache protocol (`/ac/<sha256>`).

use super::client::{HttpRemoteClient, format_reqwest_error};
use crate::error::{Error, Result};
use async_trait::async_trait;
use bazel_remote_apis::build::bazel::remote::execution::v2 as pb;
use cuenv_cas::{ActionCache, ActionResult, CanonicalMessage, Digest};
use prost::Message as _;
use reqwest::{Method, StatusCode};
use tracing::trace;

/// Upper bound on protobuf-encoded `ActionResult` bodies fetched over HTTP.
///
/// Prevents a hostile or misconfigured HTTP server from causing unbounded
/// memory allocation before protobuf decoding.
const MAX_ACTION_RESULT_BYTES: usize = 16 * 1024 * 1024;

/// An action cache backed by a Bazel HTTP/REST cache server.
#[derive(Debug, Clone)]
pub struct HttpActionCache {
    client: HttpRemoteClient,
}

impl HttpActionCache {
    /// Build an HTTP action cache on an existing [`HttpRemoteClient`].
    #[must_use]
    pub fn new(client: HttpRemoteClient) -> Self {
        Self { client }
    }

    async fn get(&self, action_digest: &Digest) -> Result<Option<ActionResult>> {
        let url = self.client.ac_url(action_digest)?;
        let maybe_bytes = self
            .client
            .retry_runner()
            .run("HTTP.GET.ac", || {
                let url = url.clone();
                async move {
                    let mut response = self
                        .client
                        .request(Method::GET, url)
                        .send()
                        .await
                        .map_err(|e| {
                            let retryable = e.is_timeout() || e.is_connect() || e.is_request();
                            (
                                Error::Rpc {
                                    operation: "HTTP.GET.ac",
                                    message: format_reqwest_error(&e),
                                },
                                retryable,
                            )
                        })?;

                    match response.status() {
                        StatusCode::OK => {}
                        StatusCode::NOT_FOUND => return Ok(None),
                        status => {
                            return Err((
                                Error::Rpc {
                                    operation: "HTTP.GET.ac",
                                    message: format!("HTTP status {status}"),
                                },
                                crate::retry::is_transient_http_status(status),
                            ));
                        }
                    }

                    if let Some(len) = response.content_length()
                        && usize::try_from(len).unwrap_or(usize::MAX) > MAX_ACTION_RESULT_BYTES
                    {
                        return Err((
                            Error::protocol(
                                "HTTP.GET.ac",
                                format!(
                                    "ActionResult size {len} exceeds limit of {MAX_ACTION_RESULT_BYTES} bytes"
                                ),
                            ),
                            false,
                        ));
                    }

                    let mut bytes = Vec::new();
                    while let Some(chunk) = response.chunk().await.map_err(|e| {
                        (
                            Error::Rpc {
                                operation: "HTTP.GET.ac",
                                message: format_reqwest_error(&e),
                            },
                            e.is_timeout() || e.is_body(),
                        )
                    })? {
                        let next_len = bytes.len().checked_add(chunk.len()).ok_or_else(|| {
                            (
                                Error::protocol("HTTP.GET.ac", "response size overflow"),
                                false,
                            )
                        })?;
                        if next_len > MAX_ACTION_RESULT_BYTES {
                            return Err((
                                Error::protocol(
                                    "HTTP.GET.ac",
                                    format!(
                                        "ActionResult body exceeds limit of {MAX_ACTION_RESULT_BYTES} bytes"
                                    ),
                                ),
                                false,
                            ));
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                    Ok(Some(bytes))
                }
            })
            .await?;

        let Some(bytes) = maybe_bytes else {
            trace!(action = %action_digest, "HTTP remote action cache miss");
            return Ok(None);
        };

        let proto = pb::ActionResult::decode(bytes.as_slice()).map_err(|e| {
            Error::protocol(
                "HTTP.GET.ac",
                format!("failed to decode ActionResult protobuf: {e}"),
            )
        })?;
        let result = ActionResult::from_proto(&proto)?;
        trace!(action = %action_digest, "HTTP remote action cache hit");
        Ok(Some(result))
    }

    async fn put(&self, action_digest: &Digest, result: &ActionResult) -> Result<()> {
        self.client.ensure_writable("update an action result")?;
        let url = self.client.ac_url(action_digest)?;
        let encoded = result.to_proto()?.encode_to_vec();
        self.client
            .retry_runner()
            .run("HTTP.PUT.ac", || {
                let url = url.clone();
                let encoded = encoded.clone();
                async move {
                    let response = self
                        .client
                        .request(Method::PUT, url)
                        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                        .body(encoded)
                        .send()
                        .await
                        .map_err(|e| {
                            let retryable = e.is_timeout() || e.is_connect() || e.is_request();
                            (
                                Error::Rpc {
                                    operation: "HTTP.PUT.ac",
                                    message: format_reqwest_error(&e),
                                },
                                retryable,
                            )
                        })?;

                    if !response.status().is_success() {
                        let status = response.status();
                        return Err((
                            Error::Rpc {
                                operation: "HTTP.PUT.ac",
                                message: format!("HTTP status {status}"),
                            },
                            crate::retry::is_transient_http_status(status),
                        ));
                    }
                    Ok(())
                }
            })
            .await?;
        trace!(action = %action_digest, "HTTP remote action cache updated");
        Ok(())
    }
}

#[async_trait]
impl ActionCache for HttpActionCache {
    async fn lookup(&self, action_digest: &Digest) -> cuenv_cas::Result<Option<ActionResult>> {
        Ok(self.get(action_digest).await?)
    }

    async fn update(&self, action_digest: &Digest, result: &ActionResult) -> cuenv_cas::Result<()> {
        Ok(self.put(action_digest, result).await?)
    }
}
