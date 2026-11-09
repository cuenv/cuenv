//! A small S3 client: GET, conditional PUT, DELETE and `ListObjectsV2`, one
//! attempt per call, with every outcome classified for the store.
//!
//! It follows the shared `state/http.rs` rules (no redirects, bounded
//! bodies, timeouts, `Retry-After`), signs with [`super::sigv4`], and never
//! puts the endpoint, a credential or a signing detail into an error.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use reqwest::{Method, StatusCode, Url};
use sha2::{Digest, Sha256};

use super::sigv4::{Credentials, SigningRequest, SlashEncoding};
use super::xml;
use crate::error::{InfrastructureError, Result};
use crate::state::http::MAXIMUM_ERROR_BODY_BYTES;

/// Largest object read, in bytes (the tenant size cap is enforced by the
/// store, well below this).
pub const MAXIMUM_OBJECT_BYTES: usize = 32 * 1024 * 1024;

/// Largest listing page read, in bytes.
const MAXIMUM_LIST_BYTES: usize = 4 * 1024 * 1024;

/// Most pages one listing reads.
const MAXIMUM_LIST_PAGES: usize = 1000;

/// Most keys one listing returns.
pub const MAXIMUM_LISTED_KEYS: usize = 100_000;

/// Longest server-requested wait before retrying a rate-limited request.
const MAXIMUM_RETRY_AFTER: Duration = Duration::from_secs(30);

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
#[derive(Clone, PartialEq, Eq)]
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
    /// Rate limited (429); a write may have been applied by some providers.
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

/// The inputs to one signed request.
struct SendRequest<'request> {
    method: Method,
    key: Option<&'request str>,
    query: &'request [(String, String)],
    headers: &'request [(String, String)],
    body: Vec<u8>,
}

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
    pub fn try_new(access: BucketAccess) -> Result<Self> {
        let http = crate::state::http::client(access.timeout, &access.endpoint)?;
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

    /// The validated effective bucket addressing mode.
    #[must_use]
    pub(super) const fn addressing(&self) -> Addressing {
        self.access.addressing
    }

    /// The validated signing region used by this connection.
    #[must_use]
    pub(super) fn region(&self) -> &str {
        &self.access.region
    }

    /// The endpoint, for the recovery identity only (never shown).
    #[must_use]
    pub fn endpoint(&self) -> &Url {
        &self.access.endpoint
    }

    /// The URL of `key` (or of the bucket for `None`).
    fn url(&self, key: Option<&str>) -> Result<Url> {
        let mut url = self.access.endpoint.clone();
        let encoded_key = key.map_or_else(String::new, |key| {
            super::sigv4::uri_encode(key, SlashEncoding::Preserve)
        });
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
                format!(
                    "/{}/{encoded_key}",
                    super::sigv4::uri_encode(&self.access.bucket, SlashEncoding::Encode)
                )
            }
        };
        url.set_path(&path);
        // The signed path and the sent path must be the same bytes: a
        // normalised `.`/`..` segment would sign one key and write another.
        if url.path() != path {
            return Err(InfrastructureError::configuration(format!(
                "object key {} does not survive URL normalisation",
                crate::error::strip_control_characters(key.unwrap_or_default())
            )));
        }
        Ok(url)
    }

    /// Send one signed request.
    async fn send(
        &self,
        input: SendRequest<'_>,
    ) -> Result<std::result::Result<reqwest::Response, reqwest::Error>> {
        let SendRequest {
            method,
            key,
            query,
            headers,
            body,
        } = input;
        let mut url = self.url(key)?;
        if !query.is_empty() {
            url.set_query(Some(&super::sigv4::canonical_query(query)));
        }
        let signed = super::sigv4::sign(
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
        let waited =
            crate::state::http::retry_after(response).map(|delay| delay.min(MAXIMUM_RETRY_AFTER));
        let body = crate::state::http::read_body_prefix(response, MAXIMUM_ERROR_BODY_BYTES).await;
        let error = xml::parse_error(&String::from_utf8_lossy(&body.bytes));
        let code = error.code.as_deref().unwrap_or_default();
        // A provider can echo arbitrary state and lock tokens in any response
        // field. Only fixed, recognized categories and the HTTP status are safe
        // to include in diagnostics.
        let detail = || format!("HTTP {}", status.as_u16());
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
                S3Failure::Permanent(format!("the bucket is in another region ({})", detail()))
            }
            _ if code == "NoSuchBucket" => S3Failure::Permanent(format!(
                "the bucket does not exist ({}); check `bucket` and `endpoint`",
                detail()
            )),
            _ if code == "AccessDenied" => {
                S3Failure::Permanent(format!("AccessDenied ({})", detail()))
            }
            StatusCode::NOT_FOUND => S3Failure::Missing,
            _ => S3Failure::Permanent(format!("{what}: {}", detail())),
        }
    }

    fn transport(error: &reqwest::Error) -> S3Failure {
        let category = if error.is_timeout() {
            "object-storage request timed out"
        } else if error.is_connect() {
            "object-storage connection failed"
        } else {
            "object-storage request failed"
        };
        S3Failure::Ambiguous(category.to_string())
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
        if !etag.starts_with('"')
            || !etag.ends_with('"')
            || etag.len() < 2
            || etag[1..etag.len() - 1]
                .bytes()
                .any(|byte| !(byte == 0x21 || (0x23..=0x7e).contains(&byte)))
        {
            return Err(S3Failure::Permanent(
                "the store returned an invalid ETag".into(),
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
            .send(SendRequest {
                method: Method::GET,
                key: Some(key),
                query: &[],
                headers: &[],
                body: Vec::new(),
            })
            .await?
        {
            Ok(response) => response,
            Err(error) => return Ok(Err(Self::transport(&error))),
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
        let Ok(body) = crate::state::http::read_whole_body(
            "object storage",
            &mut response,
            MAXIMUM_OBJECT_BYTES,
        )
        .await
        else {
            return Ok(Err(S3Failure::Ambiguous(
                "object-storage response could not be read within its size limit".to_string(),
            )));
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
            .send(SendRequest {
                method: Method::PUT,
                key: Some(key),
                query: &[],
                headers: &headers,
                body,
            })
            .await?
        {
            Ok(response) => response,
            Err(error) => return Ok(Err(Self::transport(&error))),
        };
        if !response.status().is_success() {
            return Ok(Err(self.failure(&mut response, "writing").await));
        }
        Ok(Self::etag(&response).map_err(|_| {
            S3Failure::Ambiguous("a successful write response did not provide a strong ETag".into())
        }))
    }

    /// Delete `key` without a condition. A missing object is not an error.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for a key that does not survive URL
    /// building.
    pub async fn delete(&self, key: &str) -> Result<Attempt<()>> {
        let mut response = match self
            .send(SendRequest {
                method: Method::DELETE,
                key: Some(key),
                query: &[],
                headers: &[],
                body: Vec::new(),
            })
            .await?
        {
            Ok(response) => response,
            Err(error) => return Ok(Err(Self::transport(&error))),
        };
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            return Ok(Ok(()));
        }
        Ok(Err(self.failure(&mut response, "deleting").await))
    }

    /// List every key under `prefix`, across pages. A response containing a
    /// key outside the requested prefix is refused.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the bucket URL cannot be built.
    pub async fn list(&self, prefix: &str) -> Result<Attempt<Vec<String>>> {
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        let mut seen_tokens = HashSet::new();
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
                .send(SendRequest {
                    method: Method::GET,
                    key: None,
                    query: &query,
                    headers: &[],
                    body: Vec::new(),
                })
                .await?
            {
                Ok(response) => response,
                Err(error) => return Ok(Err(Self::transport(&error))),
            };
            if !response.status().is_success() {
                return Ok(Err(self.failure(&mut response, "listing").await));
            }
            let Ok(body) = crate::state::http::read_whole_body(
                "object storage",
                &mut response,
                MAXIMUM_LIST_BYTES,
            )
            .await
            else {
                return Ok(Err(S3Failure::Ambiguous(
                    "object-storage listing could not be read within its size limit".to_string(),
                )));
            };
            // A listing that does not parse must never read as an empty one:
            // the vanished-state guard and migration decide on what it holds.
            let Ok(document) = std::str::from_utf8(&body) else {
                return Ok(Err(S3Failure::Permanent(
                    "the listing response is not valid UTF-8".to_string(),
                )));
            };
            let Ok(page) = xml::parse_list(document) else {
                return Ok(Err(S3Failure::Permanent(
                    "the listing response is not a complete ListObjectsV2 result".to_string(),
                )));
            };
            if page.keys.iter().any(|key| !key.starts_with(prefix)) {
                return Ok(Err(S3Failure::Permanent(
                    "the listing contains a key outside the requested prefix".to_string(),
                )));
            }
            keys.extend(page.keys);
            if keys.len() > MAXIMUM_LISTED_KEYS {
                return Ok(Err(S3Failure::Permanent(format!(
                    "the listing holds more than {MAXIMUM_LISTED_KEYS} keys"
                ))));
            }
            match page.next_continuation_token {
                Some(next) => {
                    let digest: [u8; 32] = Sha256::digest(next.as_bytes()).into();
                    if !seen_tokens.insert(digest) {
                        return Ok(Err(S3Failure::Permanent(
                            "the listing repeated a continuation token".to_string(),
                        )));
                    }
                    token = Some(next);
                }
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
            Err(error) => Ok(Err(Self::transport(&error))),
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
