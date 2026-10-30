//! An in-process S3 stand-in for tests.
//!
//! It serves path-style requests over HTTP/1.1, conditional PUTs (`If-None-Match: *`, `If-Match`) with strong ETags,
//! GET, DELETE and paginated `ListObjectsV2`, plus injectable faults
//! (statuses and responses dropped before or after the request is applied)
//! and a mode that ignores conditions, for the capability probe.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::sigv4::uri_encode;
use super::xml::percent_decode;

/// What a fault does to the request it matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaultAction {
    /// Answer with this status and body; the request is not applied.
    Status(u16, String),
    /// Apply the request, then close the connection without answering.
    DropAfterApply,
    /// Close the connection without applying or answering.
    DropBeforeApply,
}

/// A fault for the next request that matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    /// HTTP method the fault applies to.
    pub method: String,
    /// A substring the object key must contain (`""` for any).
    pub key_contains: String,
    /// What happens.
    pub action: FaultAction,
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

#[derive(Debug, Default)]
struct Inner {
    objects: BTreeMap<String, (Vec<u8>, String)>,
    next_etag: u64,
    faults: VecDeque<Fault>,
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

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A test that panicked while holding the lock left consistent data.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Queue a fault for the next matching request.
    pub fn fault(&self, method: &str, key_contains: &str, action: FaultAction) {
        self.inner().faults.push_back(Fault {
            method: method.to_string(),
            key_contains: key_contains.to_string(),
            action,
        });
    }

    /// Ignore every precondition (a store that does not honour them).
    pub fn ignore_conditions(&self) {
        self.inner().ignore_conditions = true;
    }

    /// Serve objects to unsigned requests (a public bucket).
    pub fn make_public(&self) {
        self.inner().public = true;
    }

    /// The object at `bucket/key`, if any.
    #[must_use]
    pub fn object(&self, bucket: &str, key: &str) -> Option<Vec<u8>> {
        self.inner()
            .objects
            .get(&format!("{bucket}/{key}"))
            .map(|(body, _)| body.clone())
    }

    /// Replace (or create) an object directly.
    pub fn put_object(&self, bucket: &str, key: &str, body: Vec<u8>) {
        let mut inner = self.inner();
        inner.next_etag += 1;
        let etag = format!("\"{}\"", inner.next_etag);
        inner
            .objects
            .insert(format!("{bucket}/{key}"), (body, etag));
    }

    /// Remove an object directly.
    pub fn remove_object(&self, bucket: &str, key: &str) {
        self.inner().objects.remove(&format!("{bucket}/{key}"));
    }

    /// Every key in `bucket`, sorted.
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
    let path = percent_decode(path)?;
    let path = path.strip_prefix('/').unwrap_or(&path);
    let (bucket, key) = path.split_once('/').unwrap_or((path, ""));
    let query = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            Some((percent_decode(name)?, percent_decode(value)?))
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
                 <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
            );
            for key in page {
                let _ = write!(
                    xml,
                    "<Contents><Key>{}</Key></Contents>",
                    uri_encode(key, true)
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

async fn answer(mut stream: TcpStream, inner: Arc<Mutex<Inner>>) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    let reply = {
        let Ok(mut inner) = inner.lock() else {
            return;
        };
        inner.requests.push(RecordedRequest {
            method: request.method.clone(),
            key: request.key.clone(),
            headers: request.headers.clone(),
        });
        let fault = inner
            .faults
            .iter()
            .position(|fault| {
                fault.method == request.method && request.key.contains(&fault.key_contains)
            })
            .and_then(|index| inner.faults.remove(index));
        match fault.map(|fault| fault.action) {
            Some(FaultAction::Status(status, body)) => {
                Some((status, Vec::new(), body.into_bytes()))
            }
            Some(FaultAction::DropBeforeApply) => None,
            Some(FaultAction::DropAfterApply) => {
                let _ = apply(&mut inner, &request);
                None
            }
            None => Some(apply(&mut inner, &request)),
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
