//! [`Cas`] over the Bazel HTTP/REST remote cache protocol (`/cas/<sha256>`).

use super::client::{HttpRemoteClient, format_reqwest_error};
use crate::error::{Error, Result};
use async_trait::async_trait;
use cuenv_cas::{Cas, Digest};
use futures::stream;
use reqwest::{Body, Method, RequestBuilder, Response, StatusCode};
use sha2::{Digest as _, Sha256};
use std::io;
use std::path::Path;
use std::time::Instant;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tracing::trace;

/// Upper bound for in-memory [`Cas::get`] responses, matching `RemoteCas`.
const MAX_IN_MEMORY_BLOB_SIZE: u64 = 512 * 1024 * 1024;
const MAX_INITIAL_ALLOCATION: usize = 8 * 1024 * 1024;
const UPLOAD_CHUNK_SIZE: usize = 64 * 1024;

/// A content-addressed store backed by a Bazel HTTP/REST cache server.
#[derive(Debug, Clone)]
pub struct HttpCas {
    client: HttpRemoteClient,
}

impl HttpCas {
    /// Build an HTTP CAS client on an existing [`HttpRemoteClient`].
    #[must_use]
    pub fn new(client: HttpRemoteClient) -> Self {
        Self { client }
    }

    async fn has_blob(&self, digest: &Digest) -> Result<bool> {
        let url = self.client.cas_url(digest)?;
        self.client
            .retry_runner()
            .run("HTTP.HEAD.cas", || {
                let url = url.clone();
                async move {
                    let response = self
                        .client
                        .request(Method::HEAD, url)
                        .send()
                        .await
                        .map_err(|e| {
                            let retryable = e.is_timeout() || e.is_connect() || e.is_request();
                            (
                                Error::Rpc {
                                    operation: "HTTP.HEAD.cas",
                                    message: format_reqwest_error(&e),
                                },
                                retryable,
                            )
                        })?;

                    match response.status() {
                        StatusCode::OK => Ok(true),
                        StatusCode::NOT_FOUND => Ok(false),
                        status => Err((
                            Error::Rpc {
                                operation: "HTTP.HEAD.cas",
                                message: format!("HTTP status {status}"),
                            },
                            crate::retry::is_transient_http_status(status),
                        )),
                    }
                }
            })
            .await
    }

    async fn read_bytes(&self, digest: &Digest) -> Result<Vec<u8>> {
        digest.validate()?;
        if digest.size_bytes > MAX_IN_MEMORY_BLOB_SIZE {
            return Err(Error::protocol(
                "HTTP.GET.cas",
                format!(
                    "blob size {} exceeds cuenv's in-memory limit of {MAX_IN_MEMORY_BLOB_SIZE}",
                    digest.size_bytes
                ),
            ));
        }

        let url = self.client.cas_url(digest)?;
        let mut response = self
            .client
            .request(Method::GET, url)
            .send()
            .await
            .map_err(|e| Error::Rpc {
                operation: "HTTP.GET.cas",
                message: format_reqwest_error(&e),
            })?;

        match response.status() {
            StatusCode::OK => {}
            StatusCode::NOT_FOUND => {
                return Err(Error::Cas(cuenv_cas::Error::NotFound {
                    digest: digest.to_resource(),
                }));
            }
            status => {
                return Err(Error::Rpc {
                    operation: "HTTP.GET.cas",
                    message: format!("HTTP status {status}"),
                });
            }
        }

        let initial_capacity = usize::try_from(digest.size_bytes)
            .unwrap_or(MAX_INITIAL_ALLOCATION)
            .min(MAX_INITIAL_ALLOCATION);
        let mut bytes = Vec::with_capacity(initial_capacity);
        let mut hasher = Sha256::new();

        while let Some(chunk) = response.chunk().await.map_err(|e| Error::Rpc {
            operation: "HTTP.GET.cas",
            message: format_reqwest_error(&e),
        })? {
            let next_size = bytes
                .len()
                .checked_add(chunk.len())
                .ok_or_else(|| Error::protocol("HTTP.GET.cas", "response size overflow"))?;
            if u64::try_from(next_size).unwrap_or(u64::MAX) > digest.size_bytes {
                return Err(Error::protocol(
                    "HTTP.GET.cas",
                    format!(
                        "server sent more than the declared {} bytes",
                        digest.size_bytes
                    ),
                ));
            }
            hasher.update(&chunk);
            bytes.extend_from_slice(&chunk);
        }

        let actual = Digest {
            hash: hex::encode(hasher.finalize()),
            size_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        };
        if &actual != digest {
            return Err(Error::Cas(cuenv_cas::Error::digest_mismatch(
                digest.to_resource(),
                actual.to_resource(),
            )));
        }

        trace!(digest = %digest, "fetched blob from HTTP CAS");
        Ok(bytes)
    }

    async fn read_to_file(&self, digest: &Digest, destination: &Path) -> Result<()> {
        digest.validate()?;
        let url = self.client.cas_url(digest)?;
        let mut response = self
            .client
            .request(Method::GET, url)
            .send()
            .await
            .map_err(|e| Error::Rpc {
                operation: "HTTP.GET.cas",
                message: e.to_string(),
            })?;

        match response.status() {
            StatusCode::OK => {}
            StatusCode::NOT_FOUND => {
                return Err(Error::Cas(cuenv_cas::Error::NotFound {
                    digest: digest.to_resource(),
                }));
            }
            status => {
                return Err(Error::Rpc {
                    operation: "HTTP.GET.cas",
                    message: format!("HTTP status {status}"),
                });
            }
        }

        let mut file = tokio::fs::File::create(destination)
            .await
            .map_err(|e| Error::Cas(cuenv_cas::Error::io(e, destination, "create")))?;
        let mut hasher = Sha256::new();
        let mut written: u64 = 0;

        let stream_result: Result<()> = async {
            while let Some(chunk) = response.chunk().await.map_err(|e| Error::Rpc {
                operation: "HTTP.GET.cas",
                message: e.to_string(),
            })? {
                let chunk_len = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
                let next_written = written
                    .checked_add(chunk_len)
                    .ok_or_else(|| Error::protocol("HTTP.GET.cas", "response size overflow"))?;
                if next_written > digest.size_bytes {
                    return Err(Error::protocol(
                        "HTTP.GET.cas",
                        format!(
                            "server sent more than the declared {} bytes",
                            digest.size_bytes
                        ),
                    ));
                }
                file.write_all(&chunk)
                    .await
                    .map_err(|e| Error::Cas(cuenv_cas::Error::io(e, destination, "write")))?;
                hasher.update(&chunk);
                written = next_written;
            }
            file.flush()
                .await
                .map_err(|e| Error::Cas(cuenv_cas::Error::io(e, destination, "flush")))?;

            let actual = Digest {
                hash: hex::encode(hasher.finalize()),
                size_bytes: written,
            };
            if &actual != digest {
                return Err(Error::Cas(cuenv_cas::Error::digest_mismatch(
                    digest.to_resource(),
                    actual.to_resource(),
                )));
            }
            Ok(())
        }
        .await;

        if let Err(err) = stream_result {
            drop(file);
            let _ = tokio::fs::remove_file(destination).await;
            return Err(err);
        }

        trace!(digest = %digest, "streamed blob from HTTP CAS to file");
        Ok(())
    }

    async fn write_bytes(&self, bytes: &[u8]) -> Result<Digest> {
        self.client.ensure_writable("upload a CAS blob")?;
        let digest = Digest::of_bytes(bytes);
        let request = self
            .client
            .request(Method::PUT, self.client.cas_url(&digest)?);
        let response = send_upload(request, &digest, Body::from(bytes.to_vec())).await?;
        check_put_status(response.status())?;
        Ok(digest)
    }

    async fn write_file(&self, source: &Path) -> Result<Digest> {
        self.client.ensure_writable("upload a CAS file")?;
        let mut file = File::open(source)
            .await
            .map_err(|error| cuenv_cas::Error::io(error, source, "open"))?;
        let digest = crate::cas::digest_file(&mut file, source).await?;
        file.rewind()
            .await
            .map_err(|error| cuenv_cas::Error::io(error, source, "rewind"))?;

        let mut url = self.client.cas_url(&digest)?;
        let mut requests_sent = 0;
        let started = Instant::now();
        loop {
            let remaining = self
                .client
                .config()
                .timeout
                .saturating_sub(started.elapsed());
            let request = self.client.streaming_put(url)?.timeout(remaining);
            let (body, mut verified) = verified_file_body(file, digest.clone());
            let response = send_upload(request, &digest, body).await?;
            requests_sent += 1;
            if let Some(redirect) = self.client.put_redirect_target(&response, requests_sent)? {
                url = redirect;
                // An early redirect can arrive while the first body is still
                // being polled. Each attempt owns its own file offset.
                file = File::open(source)
                    .await
                    .map_err(|error| cuenv_cas::Error::io(error, source, "reopen"))?;
                continue;
            }
            check_put_status(response.status())?;
            if verified.try_recv().is_err() {
                return Err(Error::protocol(
                    "HTTP.PUT.cas",
                    "server acknowledged the upload before the file body was verified",
                ));
            }
            return Ok(digest);
        }
    }
}

async fn send_upload(request: RequestBuilder, digest: &Digest, body: Body) -> Result<Response> {
    request
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .header(reqwest::header::CONTENT_LENGTH, digest.size_bytes)
        .body(body)
        .send()
        .await
        .map_err(|error| Error::Rpc {
            operation: "HTTP.PUT.cas",
            message: format_reqwest_error(&error),
        })
}

fn check_put_status(status: StatusCode) -> Result<()> {
    if status.is_success() {
        Ok(())
    } else {
        Err(Error::Rpc {
            operation: "HTTP.PUT.cas",
            message: format!("HTTP status {status}"),
        })
    }
}

struct FileUpload {
    file: File,
    digest: Digest,
    remaining: u64,
    hasher: Sha256,
    verified: oneshot::Sender<()>,
}

impl FileUpload {
    async fn next_chunk(mut self) -> io::Result<(Vec<u8>, Option<Self>)> {
        let count = usize::try_from(self.remaining)
            .unwrap_or(usize::MAX)
            .min(UPLOAD_CHUNK_SIZE);
        let mut chunk = vec![0; count];
        self.file.read_exact(&mut chunk).await?;
        self.hasher.update(&chunk);
        self.remaining -= count as u64;
        if self.remaining != 0 {
            return Ok((chunk, Some(self)));
        }

        // Withhold the last bytes until the request body has been verified.
        // An incomplete Content-Length body cannot be committed by the server.
        if self.file.read(&mut [0_u8; 1]).await? != 0
            || hex::encode(self.hasher.finalize()) != self.digest.hash
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "CAS upload digest mismatch: source changed during streaming",
            ));
        }
        let _ = self.verified.send(());
        Ok((chunk, None))
    }
}

fn verified_file_body(file: File, digest: Digest) -> (Body, oneshot::Receiver<()>) {
    let (verified, confirmation) = oneshot::channel();
    if digest.size_bytes == 0 {
        // A zero-length body already matches its digest and may never be polled.
        let _ = verified.send(());
        return (Body::default(), confirmation);
    }
    let upload = FileUpload {
        remaining: digest.size_bytes,
        file,
        digest,
        hasher: Sha256::new(),
        verified,
    };
    let chunks = stream::try_unfold(Some(upload), |state| async {
        match state {
            Some(upload) => upload.next_chunk().await.map(Some),
            None => Ok(None),
        }
    });
    (Body::wrap_stream(chunks), confirmation)
}

#[async_trait]
impl Cas for HttpCas {
    async fn contains(&self, digest: &Digest) -> cuenv_cas::Result<bool> {
        Ok(self.has_blob(digest).await?)
    }

    async fn get(&self, digest: &Digest) -> cuenv_cas::Result<Vec<u8>> {
        Ok(self.read_bytes(digest).await?)
    }

    async fn get_to_file(&self, digest: &Digest, destination: &Path) -> cuenv_cas::Result<()> {
        Ok(self.read_to_file(digest, destination).await?)
    }

    async fn put_bytes(&self, bytes: &[u8]) -> cuenv_cas::Result<Digest> {
        Ok(self.write_bytes(bytes).await?)
    }

    async fn put_file(&self, source: &Path) -> cuenv_cas::Result<Digest> {
        Ok(self.write_file(source).await?)
    }
}
