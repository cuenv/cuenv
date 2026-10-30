//! A small S3 client: GET, conditional PUT, DELETE and `ListObjectsV2`, one
//! attempt per call, with every outcome classified for the store.
//!
//! It follows the shared `state/http.rs` rules (no redirects, bounded
//! bodies, timeouts, `Retry-After`), signs with [`super::sigv4`], and never
//! puts the endpoint, a credential or a signing detail into an error.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use reqwest::{Method, StatusCode, Url};

use super::sigv4::{Credentials, SigningRequest, canonical_query, sign, uri_encode};
use super::xml;
use crate::error::{InfrastructureError, Result, strip_control_characters};
use crate::state::http::{
    MAXIMUM_ERROR_BODY_BYTES, client, describe_transport_error, read_body_prefix, retry_after,
};

/// Largest object read, in bytes (the tenant size cap is enforced by the
/// store, well below this).
pub const MAXIMUM_OBJECT_BYTES: usize = 32 * 1024 * 1024;

/// Largest listing page read, in bytes.
const MAXIMUM_LIST_BYTES: usize = 4 * 1024 * 1024;

/// Most pages one listing reads.
const MAXIMUM_LIST_PAGES: usize = 1000;

/// Most keys one listing returns.
pub const MAXIMUM_LISTED_KEYS: usize = 100_000;

/// How the bucket appears in request URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Addressing {
    /// `https://<bucket>.<endpoint host>/<key>`.
    VirtualHosted,
    /// `https://<endpoint host>/<bucket>/<key>`.
    Path,
}

/// Everything needed to reach a bucket.
#[derive(Debug, Clone)]
pub struct BucketAccess {
    /// Endpoint origin (`scheme://host[:port]`), already validated.
    pub endpoint: Url,
    /// Bucket name, already validated.
    pub bucket: String,
    /// Signing region.
    pub region: String,
    /// How the bucket appears in URLs.
    pub addressing: Addressing,
    /// Credentials.
    pub credentials: Credentials,
    /// Least time between two writes to the same key.
    pub minimum_write_interval: Duration,
    /// Per-request timeout.
    pub timeout: Duration,
}

/// The precondition of a PUT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// No precondition (lock markers and history pruning only).
    None,
    /// Create only: `If-None-Match: *`.
    Absent,
    /// Replace only this version: `If-Match: <etag>`.
    Matches(String),
}

/// An object read from the bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObject {
    /// The body.
    pub body: Vec<u8>,
    /// The ETag, exactly as the store sent it.
    pub etag: String,
}

/// How a request failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S3Failure {
    /// The precondition did not hold (412).
    PreconditionFailed,
    /// `If-Match` named an object that does not exist (AWS answers 404).
    Missing,
    /// The request may or may not have been applied: the response was lost,
    /// timed out, or was a 5xx or a 409 conflict. A write must be resolved
    /// by reading the object back.
    Ambiguous(String),
    /// Rate limited (429): the write was not applied.
    RateLimited(Option<Duration>),
    /// Anything that will not change by retrying: access denied, an expired
    /// token, a missing bucket, a wrong region, a malformed request.
    Permanent(String),
}

impl S3Failure {
    /// The failure as an error of the store.
    #[must_use]
    pub fn into_error(self, what: &str) -> InfrastructureError {
        match self {
            Self::PreconditionFailed => InfrastructureError::state(format!(
                "object storage refused {what}: its precondition did not hold"
            )),
            Self::Missing => {
                InfrastructureError::state(format!("object storage: {what} found no object"))
            }
            Self::Ambiguous(message) | Self::Permanent(message) => {
                InfrastructureError::state(format!("object storage: {what} failed: {message}"))
            }
            Self::RateLimited(_) => InfrastructureError::state(format!(
                "object storage rate-limited {what} (429); try again shortly"
            )),
        }
    }
}

/// The result of one request.
pub type Attempt<Value> = std::result::Result<Value, S3Failure>;

/// A bucket client.
#[derive(Debug)]
pub struct S3Client {
    http: reqwest::Client,
    access: BucketAccess,
    last_writes: Mutex<HashMap<String, tokio::time::Instant>>,
}

impl S3Client {
    /// A client for `access`.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::State`] when the HTTP client cannot be
    /// built.
    pub fn new(access: BucketAccess) -> Result<Self> {
        let http = client(access.timeout, &access.endpoint)?;
        Ok(Self {
            http,
            access,
            last_writes: Mutex::new(HashMap::new()),
        })
    }

    /// The bucket name.
    #[must_use]
    pub fn bucket(&self) -> &str {
        &self.access.bucket
    }

    /// The endpoint, for the recovery identity only (never shown).
    #[must_use]
    pub fn endpoint(&self) -> &Url {
        &self.access.endpoint
    }

    /// Remove every credential from `text`.
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        self.access
            .credentials
            .secrets()
            .filter(|secret| !secret.is_empty())
            .fold(text.to_string(), |text, secret| {
                text.replace(secret, "<redacted>")
            })
    }

    /// The URL of `key` (or of the bucket for `None`).
    fn url(&self, key: Option<&str>) -> Result<Url> {
        let mut url = self.access.endpoint.clone();
        let encoded_key = key.map_or_else(String::new, |key| uri_encode(key, true));
        let path = match self.access.addressing {
            Addressing::VirtualHosted => {
                let host = url.host_str().ok_or_else(|| {
                    InfrastructureError::configuration("the object-storage endpoint has no host")
                })?;
                url.set_host(Some(&format!("{}.{host}", self.access.bucket)))
                    .map_err(|_| {
                        InfrastructureError::configuration(
                            "the bucket name does not form a valid host name; use path addressing",
                        )
                    })?;
                format!("/{encoded_key}")
            }
            Addressing::Path => {
                format!("/{}/{encoded_key}", uri_encode(&self.access.bucket, false))
            }
        };
        url.set_path(&path);
        // The signed path and the sent path must be the same bytes: a
        // normalised `.`/`..` segment would sign one key and write another.
        if url.path() != path {
            return Err(InfrastructureError::configuration(format!(
                "object key {} does not survive URL normalisation",
                strip_control_characters(key.unwrap_or_default())
            )));
        }
        Ok(url)
    }

    /// Send one signed request.
    async fn send(
        &self,
        method: Method,
        key: Option<&str>,
        query: &[(String, String)],
        headers: &[(String, String)],
        body: Vec<u8>,
    ) -> Result<std::result::Result<reqwest::Response, reqwest::Error>> {
        let mut url = self.url(key)?;
        if !query.is_empty() {
            url.set_query(Some(&canonical_query(query)));
        }
        let signed = sign(
            &SigningRequest {
                method: method.as_str(),
                url: url.as_str(),
                headers,
                body: &body,
                time: std::time::SystemTime::now(),
                region: &self.access.region,
            },
            &self.access.credentials,
        )?;
        let mut request = self.http.request(method, url);
        for (name, value) in headers.iter().chain(signed.iter()) {
            request = request.header(name.as_str(), value.as_str());
        }
        Ok(request.body(body).send().await)
    }

    /// Classify a failed response.
    async fn failure(&self, response: &mut reqwest::Response, what: &str) -> S3Failure {
        let status = response.status();
        let waited = retry_after(response);
        let region = response
            .headers()
            .get("x-amz-bucket-region")
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let body = read_body_prefix(response, MAXIMUM_ERROR_BODY_BYTES).await;
        let error = xml::parse_error(&String::from_utf8_lossy(&body.bytes));
        let code = error.code.clone().unwrap_or_default();
        let detail = || {
            let message = error
                .message
                .as_deref()
                .map(|message| self.redact(&strip_control_characters(message)))
                .unwrap_or_default();
            let code = strip_control_characters(&code);
            match (code.is_empty(), message.is_empty()) {
                (true, true) => format!("HTTP {}", status.as_u16()),
                (false, true) => format!("HTTP {}, {code}", status.as_u16()),
                (true, false) => format!("HTTP {}: {message}", status.as_u16()),
                (false, false) => format!("HTTP {}, {code}: {message}", status.as_u16()),
            }
        };
        match status {
            StatusCode::PRECONDITION_FAILED => S3Failure::PreconditionFailed,
            StatusCode::TOO_MANY_REQUESTS => S3Failure::RateLimited(waited),
            StatusCode::CONFLICT => S3Failure::Ambiguous(detail()),
            status if status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT => {
                S3Failure::Ambiguous(detail())
            }
            _ if code == "ExpiredToken" || code == "TokenRefreshRequired" => {
                S3Failure::Permanent(format!(
                    "the credentials' session token has expired ({}); renew them and record \
                     anything left unrecorded with `state recover`",
                    detail()
                ))
            }
            _ if code == "PermanentRedirect"
                || code == "AuthorizationHeaderMalformed"
                || status == StatusCode::MOVED_PERMANENTLY =>
            {
                let region = region.or(error.region.clone());
                S3Failure::Permanent(match region {
                    Some(region) => format!(
                        "the bucket is in region {}, not the configured one ({})",
                        strip_control_characters(&region),
                        detail()
                    ),
                    None => format!("the bucket is in another region ({})", detail()),
                })
            }
            _ if code == "NoSuchBucket" => S3Failure::Permanent(format!(
                "the bucket does not exist ({}); check `bucket` and `endpoint`",
                detail()
            )),
            StatusCode::NOT_FOUND => S3Failure::Missing,
            _ => S3Failure::Permanent(format!("{what}: {}", detail())),
        }
    }

    fn transport(&self, error: reqwest::Error) -> S3Failure {
        // Never the URL: it names the endpoint.
        let error = error.without_url();
        S3Failure::Ambiguous(self.redact(&describe_transport_error(&error)))
    }

    fn etag(response: &reqwest::Response) -> Attempt<String> {
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string)
            .ok_or_else(|| S3Failure::Permanent("the response carried no ETag".to_string()))?;
        if etag.starts_with("W/") {
            return Err(S3Failure::Permanent(
                "the store returned a weak ETag, which a caching layer in front of it produces; \
                 use the store's S3 API endpoint, not a cached domain"
                    .to_string(),
            ));
        }
        Ok(etag)
    }

    /// Read `key`. `Ok(None)` when it does not exist.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for a key that does not survive URL
    /// building.
    pub async fn get(&self, key: &str) -> Result<Attempt<Option<StoredObject>>> {
        let mut response = match self
            .send(Method::GET, Some(key), &[], &[], Vec::new())
            .await?
        {
            Ok(response) => response,
            Err(error) => return Ok(Err(self.transport(error))),
        };
        if response.status() == StatusCode::NOT_FOUND {
            let failure = self.failure(&mut response, "reading").await;
            return Ok(match failure {
                S3Failure::Missing => Ok(None),
                other => Err(other),
            });
        }
        if !response.status().is_success() {
            return Ok(Err(self.failure(&mut response, "reading").await));
        }
        let etag = match Self::etag(&response) {
            Ok(etag) => etag,
            Err(failure) => return Ok(Err(failure)),
        };
        let body = match crate::state::http::read_whole_body(
            "object storage",
            &mut response,
            MAXIMUM_OBJECT_BYTES,
        )
        .await
        {
            Ok(body) => body,
            Err(failure) => return Ok(Err(S3Failure::Ambiguous(self.redact(&failure.message)))),
        };
        Ok(Ok(Some(StoredObject { body, etag })))
    }

    /// Wait until a write to `key` respects the minimum interval, and note
    /// this write.
    async fn pace(&self, key: &str) {
        let interval = self.access.minimum_write_interval;
        if interval.is_zero() {
            return;
        }
        let wait = {
            let Ok(mut writes) = self.last_writes.lock() else {
                return;
            };
            let now = tokio::time::Instant::now();
            let next = writes
                .get(key)
                .map_or(now, |last| (*last + interval).max(now));
            writes.insert(key.to_string(), next);
            next.saturating_duration_since(now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    /// Write `body` to `key` under `condition`. Returns the new ETag.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for a key that does not survive URL
    /// building.
    pub async fn put(
        &self,
        key: &str,
        body: Vec<u8>,
        condition: &Condition,
    ) -> Result<Attempt<String>> {
        self.pace(key).await;
        let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
        match condition {
            Condition::None => {}
            Condition::Absent => headers.push(("if-none-match".to_string(), "*".to_string())),
            Condition::Matches(etag) => headers.push(("if-match".to_string(), etag.clone())),
        }
        let mut response = match self
            .send(Method::PUT, Some(key), &[], &headers, body)
            .await?
        {
            Ok(response) => response,
            Err(error) => return Ok(Err(self.transport(error))),
        };
        if !response.status().is_success() {
            return Ok(Err(self.failure(&mut response, "writing").await));
        }
        Ok(Self::etag(&response))
    }

    /// Delete `key` without a condition. A missing object is not an error.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for a key that does not survive URL
    /// building.
    pub async fn delete(&self, key: &str) -> Result<Attempt<()>> {
        let mut response = match self
            .send(Method::DELETE, Some(key), &[], &[], Vec::new())
            .await?
        {
            Ok(response) => response,
            Err(error) => return Ok(Err(self.transport(error))),
        };
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            return Ok(Ok(()));
        }
        Ok(Err(self.failure(&mut response, "deleting").await))
    }

    /// List every key under `prefix`, across pages. Every key returned
    /// starts with `prefix`; anything else is dropped.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the bucket URL cannot be built.
    pub async fn list(&self, prefix: &str) -> Result<Attempt<Vec<String>>> {
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAXIMUM_LIST_PAGES {
            let mut query = vec![
                ("list-type".to_string(), "2".to_string()),
                ("prefix".to_string(), prefix.to_string()),
                ("encoding-type".to_string(), "url".to_string()),
            ];
            if let Some(token) = &token {
                query.push(("continuation-token".to_string(), token.clone()));
            }
            let mut response = match self
                .send(Method::GET, None, &query, &[], Vec::new())
                .await?
            {
                Ok(response) => response,
                Err(error) => return Ok(Err(self.transport(error))),
            };
            if !response.status().is_success() {
                return Ok(Err(self.failure(&mut response, "listing").await));
            }
            let body = match crate::state::http::read_whole_body(
                "object storage",
                &mut response,
                MAXIMUM_LIST_BYTES,
            )
            .await
            {
                Ok(body) => body,
                Err(failure) => {
                    return Ok(Err(S3Failure::Ambiguous(self.redact(&failure.message))));
                }
            };
            // A listing that does not parse must never read as an empty one:
            // the vanished-state guard and migration decide on what it holds.
            let Ok(page) = xml::parse_list(&String::from_utf8_lossy(&body)) else {
                return Ok(Err(S3Failure::Permanent(
                    "the listing response is not a ListObjectsV2 result".to_string(),
                )));
            };
            keys.extend(page.keys.into_iter().filter(|key| key.starts_with(prefix)));
            if keys.len() > MAXIMUM_LISTED_KEYS {
                return Ok(Err(S3Failure::Permanent(format!(
                    "the listing holds more than {MAXIMUM_LISTED_KEYS} keys"
                ))));
            }
            match page.next_continuation_token {
                Some(next) => token = Some(next),
                None => return Ok(Ok(keys)),
            }
        }
        Ok(Err(S3Failure::Permanent(format!(
            "the listing did not end within {MAXIMUM_LIST_PAGES} pages"
        ))))
    }

    /// Whether an unsigned GET of `key` succeeds, which means the bucket
    /// serves objects to anyone.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for a key that does not survive URL
    /// building.
    pub async fn readable_without_credentials(&self, key: &str) -> Result<Attempt<bool>> {
        let url = self.url(Some(key))?;
        match self.http.get(url).send().await {
            Ok(response) => Ok(Ok(response.status().is_success())),
            Err(error) => Ok(Err(self.transport(error))),
        }
    }
}

/// Retry-friendly helper: the failure a `Result<Attempt<_>>` carries, as an
/// error.
///
/// # Errors
///
/// Returns the classified failure as an [`InfrastructureError`].
pub fn require<Value>(attempt: Attempt<Value>, what: &str) -> Result<Value> {
    attempt.map_err(|failure| failure.into_error(what))
}
