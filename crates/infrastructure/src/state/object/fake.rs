//! An in-process S3 stand-in for tests.
//!
//! It serves path-style requests over HTTP/1.1, conditional PUTs (`If-None-Match: *`, `If-Match`) with strong ETags,
//! GET, DELETE and paginated `ListObjectsV2`, plus injectable faults
//! (statuses and responses dropped before or after the request is applied)
//! and a mode that ignores conditions, for the capability probe.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use super::sigv4::SlashEncoding;

/// What a fault does to the request it matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaultAction {
    /// Answer with this status and body; the request is not applied.
    Status(u16, String),
    /// Answer with this status, response headers and body without applying the request.
    StatusWithHeaders {
        /// HTTP status.
        status: u16,
        /// Response headers.
        headers: Vec<(String, String)>,
        /// Response body.
        body: String,
    },
    /// Answer with this status and raw body without applying the request.
    StatusBytes(u16, Vec<u8>),
    /// Apply the request, then close the connection without answering.
    DropAfterApply,
    /// Close the connection without applying or answering.
    DropBeforeApply,
    /// Apply the request but omit its ETag from the response.
    MissingEtagAfterApply,
    /// Apply the request but return a weak ETag.
    WeakEtagAfterApply,
    /// Apply the request, then answer with this status and body.
    StatusAfterApply(u16, String),
}

/// A fault for the next request that matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    /// HTTP method the fault applies to.
    pub method: String,
    /// Which object key the fault applies to.
    pub key: FaultKey,
    /// What happens.
    pub action: FaultAction,
}

/// How a fault selects an object key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaultKey {
    /// Match a substring of the object key (`""` matches any key).
    Contains(String),
    /// Match the exact object key (`""` matches a bucket listing).
    Exact(String),
}

/// One request the server saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    /// HTTP method.
    pub method: String,
    /// Object key (decoded), empty for bucket requests.
    pub key: String,
    /// Lower-case header names and values.
    pub headers: Vec<(String, String)>,
}

/// Where to pause the next matching request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarrierPosition {
    /// Pause before applying the request or its injected fault.
    BeforeApply,
    /// Pause after handling the request, before sending or dropping its response.
    AfterApply,
}

/// Why a request barrier could not be observed or released.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BarrierError {
    /// The matching request did not arrive within the caller's deadline.
    #[error("timed out waiting for the fake S3 request")]
    WaitTimeout,
    /// The server did not acknowledge the release within the caller's deadline.
    #[error("timed out waiting for the fake S3 request to resume")]
    ReleaseTimeout,
    /// The paused request reached the server's safety deadline.
    #[error("the fake S3 request barrier expired before it was released")]
    Expired,
    /// The server closed the barrier before completing it.
    #[error("the fake S3 request barrier closed")]
    Closed,
    /// A barrier can only report its matching request once.
    #[error("the fake S3 request was already observed")]
    AlreadyObserved,
}

/// A one-shot request barrier. Dropping it lets a paused request continue.
#[derive(Debug)]
pub struct RequestBarrier {
    reached: Option<oneshot::Receiver<RecordedRequest>>,
    release: oneshot::Sender<()>,
    resumed: oneshot::Receiver<Result<(), BarrierError>>,
}

impl RequestBarrier {
    /// Wait for the matching request to reach the barrier.
    ///
    /// # Errors
    ///
    /// Returns an error when the deadline expires, the barrier closes, or the
    /// request was already observed. A timed-out wait can be retried.
    pub async fn reached(&mut self, deadline: Duration) -> Result<RecordedRequest, BarrierError> {
        let receiver = self.reached.as_mut().ok_or(BarrierError::AlreadyObserved)?;
        match tokio::time::timeout(deadline, receiver).await {
            Ok(result) => {
                self.reached = None;
                result.map_err(|_| BarrierError::Closed)
            }
            Err(_) => Err(BarrierError::WaitTimeout),
        }
    }

    /// Resume the request and wait until the server acknowledges the release.
    ///
    /// # Errors
    ///
    /// Returns an error when the deadline expires, the barrier expired on the
    /// server, or the server closed it before acknowledging the release.
    pub async fn release(self, deadline: Duration) -> Result<(), BarrierError> {
        let _ = self.release.send(());
        match tokio::time::timeout(deadline, self.resumed).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(BarrierError::Closed),
            Err(_) => Err(BarrierError::ReleaseTimeout),
        }
    }
}

/// An unclaimed barrier, or one whose matching request is paused.
#[derive(Debug)]
struct PendingBarrier {
    method: String,
    key_contains: String,
    position: BarrierPosition,
    reached: oneshot::Sender<RecordedRequest>,
    release: oneshot::Receiver<()>,
    resumed: oneshot::Sender<Result<(), BarrierError>>,
}

impl PendingBarrier {
    async fn pause(self, request: RecordedRequest) {
        if self.reached.send(request).is_err() {
            // The test dropped its handle before the request arrived.
            return;
        }
        let result = match tokio::time::timeout(Duration::from_secs(30), self.release).await {
            Ok(Ok(())) => Ok(()),
            // Dropping the handle releases a request already at the barrier.
            Ok(Err(_)) => return,
            Err(_) => Err(BarrierError::Expired),
        };
        let _ = self.resumed.send(result);
    }
}

#[derive(Debug, Default)]
struct Inner {
    objects: BTreeMap<String, (Vec<u8>, String)>,
    next_etag: u64,
    faults: VecDeque<Fault>,
    barriers: VecDeque<PendingBarrier>,
    ignore_conditions: bool,
    public: bool,
    page_size: usize,
    requests: Vec<RecordedRequest>,
}

/// A running fake S3 server.
#[derive(Debug, Clone)]
pub struct FakeS3 {
    /// `http://127.0.0.1:<port>`.
    pub endpoint: String,
    inner: Arc<Mutex<Inner>>,
}

impl FakeS3 {
    /// Start a server on a free loopback port.
    ///
    /// # Errors
    ///
    /// Returns an error when no loopback port can be bound.
    pub async fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let inner = Arc::new(Mutex::new(Inner {
            page_size: 2,
            ..Inner::default()
        }));
        let shared = Arc::clone(&inner);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(answer(stream, Arc::clone(&shared)));
            }
        });
        Ok(Self { endpoint, inner })
    }

    /// A panic while holding the lock may leave the fixture's state incomplete.
    #[expect(
        clippy::expect_used,
        reason = "a poisoned test fixture must not return potentially inconsistent state"
    )]
    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("fake S3 state mutex poisoned")
    }

    /// Queue a fault for the next matching request.
    ///
    /// # Panics
    ///
    /// Panics if the server's state mutex is poisoned.
    pub fn fault(&self, method: &str, key_contains: &str, action: FaultAction) {
        self.inner().faults.push_back(Fault {
            method: method.to_string(),
            key: FaultKey::Contains(key_contains.to_string()),
            action,
        });
    }

    /// Queue a fault for the next request with the exact object key.
    ///
    /// # Panics
    ///
    /// Panics if the server's state mutex is poisoned.
    pub fn fault_exact(&self, method: &str, key: &str, action: FaultAction) {
        self.inner().faults.push_back(Fault {
            method: method.to_string(),
            key: FaultKey::Exact(key.to_string()),
            action,
        });
    }

    /// Pause the next request with this method and key substring.
    ///
    /// Barriers are claimed in registration order. The server automatically
    /// resumes if the handle is dropped or the pause lasts 30 seconds.
    /// `AfterApply` pauses after the selected fault is handled; a fault may
    /// prevent the request itself from being applied.
    ///
    /// # Panics
    ///
    /// Panics if the server's state mutex is poisoned.
    #[must_use]
    pub fn pause_next(
        &self,
        method: &str,
        key_contains: &str,
        position: BarrierPosition,
    ) -> RequestBarrier {
        let (reached, reached_receiver) = oneshot::channel();
        let (release, release_receiver) = oneshot::channel();
        let (resumed, resumed_receiver) = oneshot::channel();
        self.inner().barriers.push_back(PendingBarrier {
            method: method.to_string(),
            key_contains: key_contains.to_string(),
            position,
            reached,
            release: release_receiver,
            resumed,
        });
        RequestBarrier {
            reached: Some(reached_receiver),
            release,
            resumed: resumed_receiver,
        }
    }

    /// Ignore every precondition (a store that does not honour them).
    ///
    /// # Panics
    ///
    /// Panics if the server's state mutex is poisoned.
    pub fn ignore_conditions(&self) {
        self.inner().ignore_conditions = true;
    }

    /// Serve objects to unsigned requests (a public bucket).
    ///
    /// # Panics
    ///
    /// Panics if the server's state mutex is poisoned.
    pub fn make_public(&self) {
        self.inner().public = true;
    }

    /// The object at `bucket/key`, if any.
    ///
    /// # Panics
    ///
    /// Panics if the server's state mutex is poisoned.
    #[must_use]
    pub fn object(&self, bucket: &str, key: &str) -> Option<Vec<u8>> {
        self.inner()
            .objects
            .get(&format!("{bucket}/{key}"))
            .map(|(body, _)| body.clone())
    }

    /// Replace (or create) an object directly.
    ///
    /// # Panics
    ///
    /// Panics if the server's state mutex is poisoned.
    pub fn put_object(&self, bucket: &str, key: &str, body: Vec<u8>) {
        let mut inner = self.inner();
        inner.next_etag += 1;
        let etag = format!("\"{}\"", inner.next_etag);
        inner
            .objects
            .insert(format!("{bucket}/{key}"), (body, etag));
    }

    /// Remove an object directly.
    ///
    /// # Panics
    ///
    /// Panics if the server's state mutex is poisoned.
    pub fn remove_object(&self, bucket: &str, key: &str) {
        self.inner().objects.remove(&format!("{bucket}/{key}"));
    }

    /// Every key in `bucket`, sorted.
    ///
    /// # Panics
    ///
    /// Panics if the server's state mutex is poisoned.
    #[must_use]
    pub fn keys(&self, bucket: &str) -> Vec<String> {
        let prefix = format!("{bucket}/");
        self.inner()
            .objects
            .keys()
            .filter_map(|key| key.strip_prefix(&prefix).map(ToString::to_string))
            .collect()
    }

    /// Every request seen so far.
    ///
    /// # Panics
    ///
    /// Panics if the server's state mutex is poisoned.
    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.inner().requests.clone()
    }
}

/// The parsed request.
struct Request {
    method: String,
    bucket: String,
    key: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }

    fn parameter(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(parameter, _)| parameter == name)
            .map(|(_, value)| value.as_str())
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8192];
    let header_end = loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_string();
    let target = request_line.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    while buffer.len() < header_end + length {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body = buffer[header_end..header_end + length].to_vec();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let path = super::xml::percent_decode(path)?;
    let path = path.strip_prefix('/').unwrap_or(&path);
    let (bucket, key) = path.split_once('/').unwrap_or((path, ""));
    let query = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            Some((
                super::xml::percent_decode(name)?,
                super::xml::percent_decode(value)?,
            ))
        })
        .collect();
    Some(Request {
        method,
        bucket: bucket.to_string(),
        key: key.to_string(),
        query,
        headers,
        body,
    })
}

fn error_body(code: &str) -> String {
    format!("<?xml version=\"1.0\"?><Error><Code>{code}</Code><Message>{code}</Message></Error>")
}

/// A reply: status, extra headers and body.
type Reply = (u16, Vec<(String, String)>, Vec<u8>);

fn apply(inner: &mut Inner, request: &Request) -> Reply {
    let full_key = format!("{}/{}", request.bucket, request.key);
    let signed = request
        .header("authorization")
        .is_some_and(|value| value.starts_with("AWS4-HMAC-SHA256 Credential="));
    match request.method.as_str() {
        "GET" if request.key.is_empty() => {
            if !signed {
                return (403, Vec::new(), error_body("AccessDenied").into_bytes());
            }
            let prefix = request.parameter("prefix").unwrap_or_default();
            let start = request
                .parameter("continuation-token")
                .and_then(|token| token.parse::<usize>().ok())
                .unwrap_or(0);
            let bucket_prefix = format!("{}/{prefix}", request.bucket);
            let matching: Vec<String> = inner
                .objects
                .keys()
                .filter(|key| key.starts_with(&bucket_prefix))
                .filter_map(|key| key.split_once('/').map(|(_, key)| key.to_string()))
                .collect();
            let page: Vec<&String> = matching.iter().skip(start).take(inner.page_size).collect();
            let truncated = start + page.len() < matching.len();
            let mut xml = String::from(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                 <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                 <EncodingType>url</EncodingType>",
            );
            for key in page {
                let _ = write!(
                    xml,
                    "<Contents><Key>{}</Key></Contents>",
                    super::sigv4::uri_encode(key, SlashEncoding::Preserve)
                );
            }
            let _ = write!(xml, "<IsTruncated>{truncated}</IsTruncated>");
            if truncated {
                let _ = write!(
                    xml,
                    "<NextContinuationToken>{}</NextContinuationToken>",
                    start + inner.page_size
                );
            }
            xml.push_str("</ListBucketResult>");
            (200, Vec::new(), xml.into_bytes())
        }
        "GET" => {
            if !signed && !inner.public {
                return (403, Vec::new(), error_body("AccessDenied").into_bytes());
            }
            match inner.objects.get(&full_key) {
                Some((body, etag)) => (200, vec![("etag".to_string(), etag.clone())], body.clone()),
                None => (404, Vec::new(), error_body("NoSuchKey").into_bytes()),
            }
        }
        "PUT" => {
            if !signed {
                return (403, Vec::new(), error_body("AccessDenied").into_bytes());
            }
            let existing = inner.objects.get(&full_key).map(|(_, etag)| etag.clone());
            if !inner.ignore_conditions {
                if request.header("if-none-match") == Some("*") && existing.is_some() {
                    return (
                        412,
                        Vec::new(),
                        error_body("PreconditionFailed").into_bytes(),
                    );
                }
                if let Some(expected) = request.header("if-match") {
                    match &existing {
                        None => {
                            return (404, Vec::new(), error_body("NoSuchKey").into_bytes());
                        }
                        Some(etag) if etag != expected => {
                            return (
                                412,
                                Vec::new(),
                                error_body("PreconditionFailed").into_bytes(),
                            );
                        }
                        Some(_) => {}
                    }
                }
            }
            inner.next_etag += 1;
            let etag = format!("\"{}\"", inner.next_etag);
            inner
                .objects
                .insert(full_key, (request.body.clone(), etag.clone()));
            (200, vec![("etag".to_string(), etag)], Vec::new())
        }
        "DELETE" => {
            if !signed {
                return (403, Vec::new(), error_body("AccessDenied").into_bytes());
            }
            inner.objects.remove(&full_key);
            (204, Vec::new(), Vec::new())
        }
        _ => (405, Vec::new(), error_body("MethodNotAllowed").into_bytes()),
    }
}

/// A request's reply, or a pause before or after it is handled.
enum Handling {
    Ready(Option<Reply>),
    Before {
        barrier: PendingBarrier,
        observed: RecordedRequest,
        fault: Option<FaultAction>,
    },
    After {
        barrier: PendingBarrier,
        observed: RecordedRequest,
        reply: Option<Reply>,
    },
}

fn apply_fault(inner: &mut Inner, request: &Request, fault: Option<FaultAction>) -> Option<Reply> {
    match fault {
        Some(FaultAction::Status(status, body)) => Some((status, Vec::new(), body.into_bytes())),
        Some(FaultAction::StatusWithHeaders {
            status,
            headers,
            body,
        }) => Some((status, headers, body.into_bytes())),
        Some(FaultAction::StatusBytes(status, body)) => Some((status, Vec::new(), body)),
        Some(FaultAction::DropBeforeApply) => None,
        Some(FaultAction::DropAfterApply) => {
            let _ = apply(inner, request);
            None
        }
        Some(FaultAction::MissingEtagAfterApply) => {
            let (status, mut headers, body) = apply(inner, request);
            headers.retain(|(name, _)| name != "etag");
            Some((status, headers, body))
        }
        Some(FaultAction::WeakEtagAfterApply) => {
            let (status, mut headers, body) = apply(inner, request);
            headers.retain(|(name, _)| name != "etag");
            headers.push(("etag".into(), "W/\"weak\"".into()));
            Some((status, headers, body))
        }
        Some(FaultAction::StatusAfterApply(status, body)) => {
            let _ = apply(inner, request);
            Some((status, Vec::new(), body.into_bytes()))
        }
        None => Some(apply(inner, request)),
    }
}

fn prepare(inner: &mut Inner, request: &Request) -> Handling {
    let observed = RecordedRequest {
        method: request.method.clone(),
        key: request.key.clone(),
        headers: request.headers.clone(),
    };
    inner.requests.push(observed.clone());
    let fault = inner
        .faults
        .iter()
        .position(|fault| {
            fault.method == request.method
                && match &fault.key {
                    FaultKey::Contains(key) => request.key.contains(key),
                    FaultKey::Exact(key) => request.key == *key,
                }
        })
        .and_then(|index| inner.faults.remove(index))
        .map(|fault| fault.action);
    inner
        .barriers
        .retain(|barrier| !barrier.reached.is_closed());
    let barrier = inner
        .barriers
        .iter()
        .position(|barrier| {
            barrier.method == request.method && request.key.contains(&barrier.key_contains)
        })
        .and_then(|index| inner.barriers.remove(index));
    match barrier {
        Some(barrier) if barrier.position == BarrierPosition::BeforeApply => Handling::Before {
            barrier,
            observed,
            fault,
        },
        Some(barrier) => Handling::After {
            barrier,
            observed,
            reply: apply_fault(inner, request, fault),
        },
        None => Handling::Ready(apply_fault(inner, request, fault)),
    }
}

fn poisoned_reply() -> Reply {
    (500, Vec::new(), b"fake S3 state mutex poisoned".to_vec())
}

async fn answer(mut stream: TcpStream, inner: Arc<Mutex<Inner>>) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    let handling = match inner.lock() {
        Ok(mut inner) => prepare(&mut inner, &request),
        Err(_) => Handling::Ready(Some(poisoned_reply())),
    };
    // The fixture mutex is never held while a barrier waits for the test.
    let reply = match handling {
        Handling::Ready(reply) => reply,
        Handling::Before {
            barrier,
            observed,
            fault,
        } => {
            barrier.pause(observed).await;
            match inner.lock() {
                Ok(mut inner) => apply_fault(&mut inner, &request, fault),
                Err(_) => Some(poisoned_reply()),
            }
        }
        Handling::After {
            barrier,
            observed,
            reply,
        } => {
            barrier.pause(observed).await;
            reply
        }
    };
    let Some((status, headers, body)) = reply else {
        // Close without answering: the client sees a lost response.
        let _ = stream.shutdown().await;
        return;
    };
    let mut head = format!(
        "HTTP/1.1 {status} Status\r\ncontent-length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        let _ = write!(head, "{name}: {value}\r\n");
    }
    head.push_str("connection: close\r\n\r\n");
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(&body).await;
    let _ = stream.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::{BarrierError, BarrierPosition, FakeS3};
    use std::time::Duration;

    use reqwest::header::{AUTHORIZATION, ETAG, IF_MATCH};

    const DEADLINE: Duration = Duration::from_secs(10);
    const AUTHENTICATION: &str = "AWS4-HMAC-SHA256 Credential=test";

    fn client() -> Result<reqwest::Client, reqwest::Error> {
        crate::ensure_rustls_cryptography_provider();
        reqwest::Client::builder()
            .no_proxy()
            .timeout(DEADLINE)
            .build()
    }

    #[tokio::test]
    async fn barrier_before_apply_allows_another_write_before_the_conditional_put()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = FakeS3::start().await?;
        let client = client()?;
        let url = format!("{}/state/project/state.json", server.endpoint);
        server.put_object("state", "project/state.json", b"original".to_vec());
        let current = client
            .get(&url)
            .header(AUTHORIZATION, AUTHENTICATION)
            .send()
            .await?;
        let etag = current.headers().get(ETAG).ok_or("missing ETag")?.clone();
        let mut barrier =
            server.pause_next("PUT", "project/state.json", BarrierPosition::BeforeApply);
        let pending = tokio::spawn({
            let client = client.clone();
            let url = url.clone();
            async move {
                client
                    .put(url)
                    .header(AUTHORIZATION, AUTHENTICATION)
                    .header(IF_MATCH, etag)
                    .body("stale")
                    .send()
                    .await
            }
        });

        let observed = barrier.reached(DEADLINE).await?;
        assert_eq!(observed.method, "PUT");
        assert_eq!(observed.key, "project/state.json");
        assert!(!pending.is_finished());
        assert_eq!(
            server.object("state", "project/state.json"),
            Some(b"original".to_vec())
        );
        let competing = client
            .put(&url)
            .header(AUTHORIZATION, AUTHENTICATION)
            .body("newer")
            .send()
            .await?;
        assert_eq!(competing.status(), reqwest::StatusCode::OK);
        barrier.release(DEADLINE).await?;
        let result = tokio::time::timeout(DEADLINE, pending).await???;
        assert_eq!(result.status(), reqwest::StatusCode::PRECONDITION_FAILED);
        assert_eq!(
            server.object("state", "project/state.json"),
            Some(b"newer".to_vec())
        );
        Ok(())
    }

    #[tokio::test]
    async fn barrier_after_apply_pauses_the_response_without_holding_the_mutex()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = FakeS3::start().await?;
        let client = client()?;
        let url = format!("{}/state/project/state.json", server.endpoint);
        let mut barrier =
            server.pause_next("PUT", "project/state.json", BarrierPosition::AfterApply);
        let pending = tokio::spawn({
            let client = client.clone();
            let url = url.clone();
            async move {
                client
                    .put(url)
                    .header(AUTHORIZATION, AUTHENTICATION)
                    .body("first")
                    .send()
                    .await
            }
        });

        barrier.reached(DEADLINE).await?;
        assert!(!pending.is_finished());
        assert_eq!(
            server.object("state", "project/state.json"),
            Some(b"first".to_vec())
        );
        let competing = client
            .put(&url)
            .header(AUTHORIZATION, AUTHENTICATION)
            .body("second")
            .send()
            .await?;
        assert_eq!(competing.status(), reqwest::StatusCode::OK);
        barrier.release(DEADLINE).await?;
        assert_eq!(
            tokio::time::timeout(DEADLINE, pending).await???.status(),
            reqwest::StatusCode::OK
        );
        assert_eq!(
            server.object("state", "project/state.json"),
            Some(b"second".to_vec())
        );
        Ok(())
    }

    #[tokio::test]
    async fn dropped_barrier_resumes_the_request_and_waits_are_bounded()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = FakeS3::start().await?;
        let client = client()?;
        let url = format!("{}/state/project/state.json", server.endpoint);
        let mut barrier =
            server.pause_next("PUT", "project/state.json", BarrierPosition::BeforeApply);
        let unmatched_key = client
            .put(format!("{}/state/other.json", server.endpoint))
            .header(AUTHORIZATION, AUTHENTICATION)
            .send()
            .await?;
        assert_eq!(unmatched_key.status(), reqwest::StatusCode::OK);
        let unmatched_method = client
            .get(&url)
            .header(AUTHORIZATION, AUTHENTICATION)
            .send()
            .await?;
        assert_eq!(unmatched_method.status(), reqwest::StatusCode::NOT_FOUND);
        assert_eq!(
            barrier.reached(Duration::from_millis(1)).await,
            Err(BarrierError::WaitTimeout)
        );
        let pending = tokio::spawn(async move {
            client
                .put(url)
                .header(AUTHORIZATION, AUTHENTICATION)
                .body("completed")
                .send()
                .await
        });
        barrier.reached(DEADLINE).await?;
        drop(barrier);
        assert_eq!(
            tokio::time::timeout(DEADLINE, pending).await???.status(),
            reqwest::StatusCode::OK
        );
        assert_eq!(
            server.object("state", "project/state.json"),
            Some(b"completed".to_vec())
        );
        Ok(())
    }

    #[tokio::test]
    async fn poisoned_fixture_does_not_return_partially_updated_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = FakeS3::start().await?;
        server.put_object("bucket", "key", b"original".to_vec());
        assert_eq!(server.object("bucket", "key"), Some(b"original".to_vec()));

        let interrupted = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let mut inner = server.inner.lock().expect("fixture starts healthy");
                    inner.objects.clear();
                    panic!("interrupt a fixture mutation");
                })
                .join()
        });
        assert!(interrupted.is_err());
        assert!(
            std::thread::scope(|scope| scope.spawn(|| server.object("bucket", "key")).join())
                .is_err(),
            "a poisoned fixture must not return partially updated data"
        );
        assert!(
            std::thread::scope(|scope| {
                scope
                    .spawn(|| server.pause_next("GET", "key", BarrierPosition::AfterApply))
                    .join()
            })
            .is_err(),
            "a poisoned fixture must not accept a new barrier"
        );
        let response = client()?
            .get(format!("{}/bucket/key", server.endpoint))
            .header(AUTHORIZATION, AUTHENTICATION)
            .send()
            .await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(response.text().await?, "fake S3 state mutex poisoned");
        Ok(())
    }
}
