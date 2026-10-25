//! HTTP plumbing shared by the remote state stores.
//!
//! Every store talks to its database over HTTPS with a bearer token. The
//! rules are the same for each: redirects are never followed (a redirect
//! could carry the token elsewhere), response bodies are read only up to a
//! fixed size, and failures are classified as transient (worth retrying) or
//! permanent.

use std::iter;
use std::time::Duration;

use super::sql::{Attempted, Failure, FailureKind};
use crate::error::{InfrastructureError, Result};

/// Largest HTTP error body read (and then only parsed for an error code,
/// never quoted unless the request carried no state), in bytes.
pub(super) const MAXIMUM_ERROR_BODY_BYTES: usize = 4096;

/// Largest successful response read, in bytes (64 MiB).
pub(super) const MAXIMUM_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Per-request timeout for state store calls.
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Build the HTTP client of a store: bounded by `timeout`, never following
/// redirects, and, for a plaintext loopback endpoint, never going through a
/// proxy that would see the token in clear.
pub(super) fn client(timeout: Duration, endpoint: &reqwest::Url) -> Result<reqwest::Client> {
    crate::ensure_rustls_cryptography_provider();
    let builder = reqwest::Client::builder()
        .timeout(timeout)
        // A redirect could carry the request, and its token, elsewhere.
        .redirect(reqwest::redirect::Policy::none());
    // Stores produce `http` only for a loopback host: talk to it directly.
    let builder = if endpoint.scheme() == "http" {
        builder.no_proxy()
    } else {
        builder
    };
    builder.build().map_err(|error| {
        InfrastructureError::state(format!(
            "failed to build HTTP client: {}",
            describe_transport_error(&error)
        ))
    })
}

/// HTTP statuses worth retrying: 408, 429 and every 5xx.
pub(super) fn status_is_transient(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

/// Classify a failure to send a request to `backend` or read its response.
///
/// Connection failures, timeouts and interrupted bodies are transient; only
/// request construction and redirect policy failures are permanent.
pub(super) fn transport_failure(backend: &str, error: &reqwest::Error) -> Failure {
    Failure::new(
        if error.is_builder() || error.is_redirect() {
            FailureKind::Permanent
        } else {
            FailureKind::Transient
        },
        format!(
            "{backend} request failed: {}",
            describe_transport_error(error)
        ),
    )
}

/// Render an HTTP client error with its whole source chain, saying explicitly
/// when it was a timeout.
pub(super) fn describe_transport_error(error: &reqwest::Error) -> String {
    let mut messages: Vec<String> = vec![error.to_string()];
    for source in iter::successors(std::error::Error::source(error), |source| source.source()) {
        let message = source.to_string();
        if messages
            .last()
            .is_none_or(|previous| !previous.contains(&message))
        {
            messages.push(message);
        }
    }
    let chain = messages.join(": ");
    if error.is_timeout() {
        format!("timed out: {chain}")
    } else {
        chain
    }
}

/// The first bytes of a response body: at most `limit`, plus whether more
/// followed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct BodyPrefix {
    pub(super) bytes: Vec<u8>,
    pub(super) truncated: bool,
}

/// Read at most `limit` bytes of an error response body, stopping there.
///
/// A body that fails part way keeps what arrived: it only decorates an error.
pub(super) async fn read_body_prefix(response: &mut reqwest::Response, limit: usize) -> BodyPrefix {
    let mut prefix = BodyPrefix::default();
    while let Ok(Some(chunk)) = response.chunk().await {
        let room = limit - prefix.bytes.len();
        if chunk.len() > room {
            prefix.bytes.extend_from_slice(&chunk[..room]);
            prefix.truncated = true;
            break;
        }
        prefix.bytes.extend_from_slice(&chunk);
    }
    prefix
}

/// Read a successful response body from `backend`, failing once it exceeds
/// `limit` bytes.
pub(super) async fn read_whole_body(
    backend: &str,
    response: &mut reqwest::Response,
    limit: usize,
) -> Attempted<Vec<u8>> {
    let too_large = || {
        Failure::permanent(format!(
            "{backend} response exceeds the {limit}-byte limit; the tenant's state is too large \
             to read in one response"
        ))
    };
    if response
        .content_length()
        .is_some_and(|length| usize::try_from(length).map_or(true, |length| length > limit))
    {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| transport_failure(backend, &error))?
    {
        if chunk.len() > limit - bytes.len() {
            return Err(too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// The delay a `Retry-After` header asks for, in whole seconds (the HTTP
/// date form is not honoured).
pub(super) fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Render an error body prefix for a message, cut on a character boundary.
pub(super) fn describe_error_body(body: &BodyPrefix, total_bytes: Option<u64>) -> String {
    let text = String::from_utf8_lossy(&body.bytes);
    if !body.truncated {
        return text.into_owned();
    }
    // A multi-byte character cut at the limit decodes as a replacement
    // character; drop it.
    let text = text.trim_end_matches(char::REPLACEMENT_CHARACTER);
    match total_bytes {
        Some(total) => format!("{text}… (truncated, {total} bytes in total)"),
        None => format!("{text}… (truncated)"),
    }
}
