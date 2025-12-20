//! Remote Cache Backend (Bazel Remote Execution API v2)
//!
//! Implements distributed caching using the Bazel Remote Execution API v2
//! for Action Cache and Content Addressable Storage (CAS).

use crate::executor::backend::{
    BackendError, BackendResult, CacheBackend, CacheEntry, CacheLookupResult, CacheOutput,
    policy_allows_read, policy_allows_write,
};
use crate::ir::{CachePolicy, OutputType, Task as IRTask};
use async_trait::async_trait;
use backoff::ExponentialBackoff;
use bazel_remote_apis::build::bazel::remote::execution::v2::{
    ActionResult, BatchReadBlobsRequest, BatchReadBlobsResponse, BatchUpdateBlobsRequest, Digest,
    GetActionResultRequest, OutputFile, UpdateActionResultRequest,
    action_cache_client::ActionCacheClient, batch_update_blobs_request,
    content_addressable_storage_client::ContentAddressableStorageClient,
};

use sha2::{Digest as Sha2Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

/// Configuration for the remote cache backend
#[derive(Debug, Clone)]
pub struct RemoteCacheConfig {
    /// Remote cache URL (e.g., "<grpc://cache.example.com:9092>")
    pub url: String,
    /// Instance name for the cache (namespace)
    pub instance_name: String,
    /// Enable TLS
    pub tls_enabled: bool,
    /// Path to TLS certificate (optional)
    pub tls_cert_path: Option<String>,
    /// Connection timeout
    pub connect_timeout: Duration,
    /// Request timeout
    pub request_timeout: Duration,
    /// Max retry attempts
    pub max_retries: u32,
}

impl Default for RemoteCacheConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            instance_name: String::new(),
            tls_enabled: false,
            tls_cert_path: None,
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(60),
            max_retries: 3,
        }
    }
}

impl RemoteCacheConfig {
    /// Create config from environment variables
    ///
    /// Reads:
    /// - `CUENV_REMOTE_CACHE_URL`: gRPC URL
    /// - `CUENV_REMOTE_CACHE_INSTANCE`: Instance name
    /// - `CUENV_REMOTE_CACHE_TLS`: Enable TLS ("true"/"false")
    /// - `CUENV_REMOTE_CACHE_TLS_CERT`: Path to TLS certificate
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let url = std::env::var("CUENV_REMOTE_CACHE_URL").ok()?;
        if url.is_empty() {
            return None;
        }

        Some(Self {
            url,
            instance_name: std::env::var("CUENV_REMOTE_CACHE_INSTANCE").unwrap_or_default(),
            tls_enabled: std::env::var("CUENV_REMOTE_CACHE_TLS")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            tls_cert_path: std::env::var("CUENV_REMOTE_CACHE_TLS_CERT").ok(),
            ..Default::default()
        })
    }
}

/// Remote cache backend using Bazel RE v2 API
pub struct RemoteCacheBackend {
    config: RemoteCacheConfig,
    channel: Arc<RwLock<Option<Channel>>>,
}

impl RemoteCacheBackend {
    /// Create a new remote cache backend
    #[must_use]
    pub fn new(config: RemoteCacheConfig) -> Self {
        Self {
            config,
            channel: Arc::new(RwLock::new(None)),
        }
    }

    /// Create from environment variables
    ///
    /// Returns `None` if `CUENV_REMOTE_CACHE_URL` is not set
    #[must_use]
    pub fn from_env() -> Option<Self> {
        RemoteCacheConfig::from_env().map(Self::new)
    }

    /// Get or create a gRPC channel
    async fn get_channel(&self) -> BackendResult<Channel> {
        // Check if we already have a connection
        {
            let guard = self.channel.read().await;
            if let Some(channel) = guard.as_ref() {
                return Ok(channel.clone());
            }
        }

        // Create new connection
        let channel = self.connect().await?;

        // Store for reuse
        {
            let mut guard = self.channel.write().await;
            *guard = Some(channel.clone());
        }

        Ok(channel)
    }

    /// Establish connection to remote cache
    async fn connect(&self) -> BackendResult<Channel> {
        let url = self.config.url.replace("grpc://", "http://");

        let mut endpoint = Endpoint::from_shared(url.clone())
            .map_err(|e| BackendError::Connection(format!("Invalid URL: {e}")))?
            .connect_timeout(self.config.connect_timeout)
            .timeout(self.config.request_timeout);

        if self.config.tls_enabled {
            let mut tls_config = ClientTlsConfig::new();
            if let Some(cert_path) = &self.config.tls_cert_path {
                let cert = tokio::fs::read(cert_path).await.map_err(|e| {
                    BackendError::Connection(format!("Failed to read TLS cert: {e}"))
                })?;
                let cert = tonic::transport::Certificate::from_pem(cert);
                tls_config = tls_config.ca_certificate(cert);
            }
            endpoint = endpoint
                .tls_config(tls_config)
                .map_err(|e| BackendError::Connection(format!("TLS config error: {e}")))?;
        }

        endpoint
            .connect()
            .await
            .map_err(|e| BackendError::Connection(format!("Failed to connect to {url}: {e}")))
    }

    /// Get Action Cache client
    async fn action_cache_client(&self) -> BackendResult<ActionCacheClient<Channel>> {
        let channel = self.get_channel().await?;
        Ok(ActionCacheClient::new(channel))
    }

    /// Get CAS client
    async fn cas_client(&self) -> BackendResult<ContentAddressableStorageClient<Channel>> {
        let channel = self.get_channel().await?;
        Ok(ContentAddressableStorageClient::new(channel))
    }

    /// Convert task digest to Bazel Digest format
    fn to_bazel_digest(digest_str: &str) -> Digest {
        let hash = digest_str.strip_prefix("sha256:").unwrap_or(digest_str);
        Digest {
            hash: hash.to_string(),
            size_bytes: 0, // Size is computed from action, not stored in our digest
        }
    }

    /// Compute SHA-256 digest of data
    fn compute_digest(data: &[u8]) -> Digest {
        let mut hasher = Sha256::new();
        hasher.update(data);
        let hash = hex::encode(hasher.finalize());
        Digest {
            hash,
            size_bytes: data.len() as i64,
        }
    }

    /// Upload blobs to CAS with retry
    async fn upload_blobs(&self, blobs: Vec<(Digest, Vec<u8>)>) -> BackendResult<()> {
        if blobs.is_empty() {
            return Ok(());
        }

        let requests: Vec<batch_update_blobs_request::Request> = blobs
            .into_iter()
            .map(|(digest, data)| batch_update_blobs_request::Request {
                digest: Some(digest),
                data,
                compressor: 0,
            })
            .collect();

        let request = BatchUpdateBlobsRequest {
            instance_name: self.config.instance_name.clone(),
            requests,
            digest_function: 0,
        };

        self.retry_with_backoff(|mut client| {
            let req = request.clone();
            async move { client.batch_update_blobs(req).await.map(|_| ()) }
        })
        .await?;

        Ok(())
    }

    /// Download blobs from CAS with retry
    async fn download_blobs(
        &self,
        digests: Vec<Digest>,
    ) -> BackendResult<HashMap<String, Vec<u8>>> {
        if digests.is_empty() {
            return Ok(HashMap::new());
        }

        let request = BatchReadBlobsRequest {
            instance_name: self.config.instance_name.clone(),
            digests,
            acceptable_compressors: vec![],
            digest_function: 0,
        };

        let response = self.retry_cas_read(request).await?;

        let mut blobs = HashMap::new();
        for resp in response.responses {
            if let Some(digest) = resp.digest {
                blobs.insert(digest.hash, resp.data);
            }
        }

        Ok(blobs)
    }

    /// Retry helper for CAS operations
    async fn retry_with_backoff<F, Fut>(&self, operation: F) -> BackendResult<()>
    where
        F: Fn(ContentAddressableStorageClient<Channel>) -> Fut,
        Fut: std::future::Future<Output = Result<(), tonic::Status>>,
    {
        let mut last_error = None;
        let mut delay = Duration::from_millis(100);

        for attempt in 0..self.config.max_retries {
            let client = self.cas_client().await?;

            match operation(client).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if !is_retryable(&e) {
                        // Non-retryable errors are hard failures (auth, permission, etc.)
                        return Err(BackendError::Connection(e.to_string()));
                    }
                    last_error = Some(e);
                    if attempt + 1 < self.config.max_retries {
                        tokio::time::sleep(delay).await;
                        delay = std::cmp::min(delay * 2, Duration::from_secs(2));
                    }
                }
            }
        }

        // Retries exhausted - this is a transient failure, allow graceful degradation
        Err(BackendError::Unavailable(last_error.map_or_else(
            || "retries exhausted".to_string(),
            |e| format!("retries exhausted: {e}"),
        )))
    }

    /// Retry helper for CAS read operations
    async fn retry_cas_read(
        &self,
        request: BatchReadBlobsRequest,
    ) -> BackendResult<BatchReadBlobsResponse> {
        let mut last_error = None;
        let mut delay = Duration::from_millis(100);

        for attempt in 0..self.config.max_retries {
            let mut client = self.cas_client().await?;

            match client.batch_read_blobs(request.clone()).await {
                Ok(response) => return Ok(response.into_inner()),
                Err(e) => {
                    if !is_retryable(&e) {
                        // Non-retryable errors are hard failures (auth, permission, etc.)
                        return Err(BackendError::Connection(e.to_string()));
                    }
                    last_error = Some(e);
                    if attempt + 1 < self.config.max_retries {
                        tokio::time::sleep(delay).await;
                        delay = std::cmp::min(delay * 2, Duration::from_secs(2));
                    }
                }
            }
        }

        // Retries exhausted - this is a transient failure, allow graceful degradation
        Err(BackendError::Unavailable(last_error.map_or_else(
            || "retries exhausted".to_string(),
            |e| format!("retries exhausted: {e}"),
        )))
    }

    /// Retry helper for action cache get operations
    async fn retry_action_cache_get(
        &self,
        request: GetActionResultRequest,
        digest: &str,
    ) -> BackendResult<tonic::Response<ActionResult>> {
        let mut last_error = None;
        let mut delay = Duration::from_millis(100);

        for attempt in 0..self.config.max_retries {
            let mut client = self.action_cache_client().await?;

            match client.get_action_result(request.clone()).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if e.code() == tonic::Code::NotFound {
                        return Err(BackendError::ActionNotFound {
                            digest: digest.to_string(),
                        });
                    }
                    if !is_retryable(&e) {
                        // Non-retryable errors are hard failures (auth, permission, etc.)
                        return Err(BackendError::Connection(e.to_string()));
                    }
                    last_error = Some(e);
                    if attempt + 1 < self.config.max_retries {
                        tokio::time::sleep(delay).await;
                        delay = std::cmp::min(delay * 2, Duration::from_secs(2));
                    }
                }
            }
        }

        // Retries exhausted - this is a transient failure, allow graceful degradation
        Err(BackendError::Unavailable(last_error.map_or_else(
            || "retries exhausted".to_string(),
            |e| format!("retries exhausted: {e}"),
        )))
    }

    /// Create exponential backoff config
    #[allow(dead_code)]
    fn create_backoff() -> ExponentialBackoff {
        ExponentialBackoff {
            initial_interval: Duration::from_millis(100),
            max_interval: Duration::from_secs(2),
            max_elapsed_time: Some(Duration::from_secs(30)),
            multiplier: 2.0,
            ..Default::default()
        }
    }
}

#[async_trait]
impl CacheBackend for RemoteCacheBackend {
    async fn check(
        &self,
        task: &IRTask,
        digest: &str,
        policy: CachePolicy,
    ) -> BackendResult<CacheLookupResult> {
        if !policy_allows_read(policy) {
            tracing::debug!(
                task = %task.id,
                policy = ?policy,
                "Remote cache lookup skipped due to policy"
            );
            return Ok(CacheLookupResult::miss(digest));
        }

        // Check if we can connect - if not, treat as miss
        if let Err(e) = self.action_cache_client().await {
            tracing::warn!(error = %e, "Remote cache unavailable, treating as miss");
            return Ok(CacheLookupResult::miss(digest));
        }

        let action_digest = Self::to_bazel_digest(digest);
        let request = GetActionResultRequest {
            instance_name: self.config.instance_name.clone(),
            action_digest: Some(action_digest),
            inline_stdout: true,
            inline_stderr: true,
            inline_output_files: vec![],
            digest_function: 0,
        };

        let result = self.retry_action_cache_get(request, digest).await;

        match result {
            Ok(response) => {
                let action_result = response.into_inner();
                // Extract execution duration from metadata if available
                let duration_ms = action_result
                    .execution_metadata
                    .as_ref()
                    .and_then(|m| {
                        let start = m.execution_start_timestamp.as_ref()?;
                        let end = m.execution_completed_timestamp.as_ref()?;
                        let start_nanos = start.seconds * 1_000_000_000 + i64::from(start.nanos);
                        let end_nanos = end.seconds * 1_000_000_000 + i64::from(end.nanos);
                        Some(((end_nanos - start_nanos) / 1_000_000) as u64)
                    })
                    .unwrap_or(0);

                tracing::debug!(
                    task = %task.id,
                    digest = %digest,
                    "Remote cache hit"
                );
                Ok(CacheLookupResult::hit(digest, duration_ms))
            }
            Err(BackendError::ActionNotFound { .. }) => {
                tracing::debug!(
                    task = %task.id,
                    digest = %digest,
                    "Remote cache miss"
                );
                Ok(CacheLookupResult::miss(digest))
            }
            Err(e) => {
                tracing::warn!(
                    task = %task.id,
                    error = %e,
                    "Remote cache check failed, treating as miss"
                );
                Ok(CacheLookupResult::miss(digest))
            }
        }
    }

    async fn store(
        &self,
        task: &IRTask,
        digest: &str,
        entry: &CacheEntry,
        policy: CachePolicy,
    ) -> BackendResult<()> {
        if !policy_allows_write(policy) {
            tracing::debug!(
                task = %task.id,
                policy = ?policy,
                "Remote cache write skipped due to policy"
            );
            return Ok(());
        }

        // Upload output files to CAS
        let mut output_files = Vec::new();
        let mut blobs_to_upload = Vec::new();

        for output in &entry.outputs {
            let digest = Self::compute_digest(&output.data);
            blobs_to_upload.push((digest.clone(), output.data.clone()));
            output_files.push(OutputFile {
                path: output.path.clone(),
                digest: Some(digest),
                is_executable: output.is_executable,
                contents: vec![], // Not inlined
                node_properties: None,
            });
        }

        // Upload stdout/stderr to CAS
        let stdout_digest = entry.stdout.as_ref().map(|s| {
            let bytes = s.as_bytes().to_vec();
            let digest = Self::compute_digest(&bytes);
            blobs_to_upload.push((digest.clone(), bytes));
            digest
        });

        let stderr_digest = entry.stderr.as_ref().map(|s| {
            let bytes = s.as_bytes().to_vec();
            let digest = Self::compute_digest(&bytes);
            blobs_to_upload.push((digest.clone(), bytes));
            digest
        });

        // Upload all blobs
        if let Err(e) = self.upload_blobs(blobs_to_upload).await {
            tracing::warn!(
                task = %task.id,
                error = %e,
                "Failed to upload blobs to CAS"
            );
            return Ok(()); // Graceful degradation
        }

        // Create and store ActionResult
        #[allow(deprecated)]
        let action_result = ActionResult {
            output_files,
            output_file_symlinks: vec![], // Deprecated but required for struct
            output_symlinks: vec![],
            output_directories: vec![],
            output_directory_symlinks: vec![], // Deprecated but required for struct
            exit_code: entry.exit_code,
            stdout_raw: vec![],
            stderr_raw: vec![],
            stdout_digest,
            stderr_digest,
            execution_metadata: None,
        };

        let mut client = match self.action_cache_client().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "Remote cache unavailable for write");
                return Ok(());
            }
        };

        let action_digest = Self::to_bazel_digest(digest);
        let request = UpdateActionResultRequest {
            instance_name: self.config.instance_name.clone(),
            action_digest: Some(action_digest),
            action_result: Some(action_result),
            results_cache_policy: None,
            digest_function: 0,
        };

        if let Err(e) = client.update_action_result(request).await {
            tracing::warn!(
                task = %task.id,
                error = %e,
                "Failed to store action result"
            );
        } else {
            tracing::debug!(
                task = %task.id,
                digest = %digest,
                "Stored in remote cache"
            );
        }

        Ok(())
    }

    async fn restore_outputs(
        &self,
        task: &IRTask,
        digest: &str,
        workspace: &Path,
    ) -> BackendResult<Vec<CacheOutput>> {
        let mut client = self.action_cache_client().await?;

        let action_digest = Self::to_bazel_digest(digest);
        let request = GetActionResultRequest {
            instance_name: self.config.instance_name.clone(),
            action_digest: Some(action_digest),
            inline_stdout: false,
            inline_stderr: false,
            inline_output_files: vec![],
            digest_function: 0,
        };

        let response = client
            .get_action_result(request)
            .await
            .map_err(|e| BackendError::Connection(e.to_string()))?;

        let action_result = response.into_inner();

        // Download output files from CAS
        let digests: Vec<Digest> = action_result
            .output_files
            .iter()
            .filter_map(|f| f.digest.clone())
            .collect();

        let blobs = self.download_blobs(digests).await?;

        let mut outputs = Vec::new();
        for output_file in &action_result.output_files {
            let Some(digest) = &output_file.digest else {
                continue;
            };

            let Some(data) = blobs.get(&digest.hash) else {
                tracing::warn!(
                    path = %output_file.path,
                    digest = %digest.hash,
                    "Output file not found in CAS"
                );
                continue;
            };

            // Only restore orchestrator outputs to workspace
            let should_restore = task
                .outputs
                .iter()
                .any(|o| o.path == output_file.path && o.output_type == OutputType::Orchestrator);

            if should_restore {
                let dest_path = workspace.join(&output_file.path);
                if let Some(parent) = dest_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&dest_path, data)?;

                #[cfg(unix)]
                if output_file.is_executable {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = std::fs::metadata(&dest_path)?.permissions();
                    perms.set_mode(perms.mode() | 0o111);
                    std::fs::set_permissions(&dest_path, perms)?;
                }
            }

            outputs.push(CacheOutput {
                path: output_file.path.clone(),
                data: data.clone(),
                is_executable: output_file.is_executable,
            });
        }

        tracing::debug!(
            task = %task.id,
            digest = %digest,
            outputs = outputs.len(),
            "Restored outputs from remote cache"
        );

        Ok(outputs)
    }

    async fn get_logs(
        &self,
        _task: &IRTask,
        digest: &str,
    ) -> BackendResult<(Option<String>, Option<String>)> {
        let mut client = self.action_cache_client().await?;

        let action_digest = Self::to_bazel_digest(digest);
        let request = GetActionResultRequest {
            instance_name: self.config.instance_name.clone(),
            action_digest: Some(action_digest),
            inline_stdout: true,
            inline_stderr: true,
            inline_output_files: vec![],
            digest_function: 0,
        };

        let response = client
            .get_action_result(request)
            .await
            .map_err(|e| BackendError::Connection(e.to_string()))?;

        let action_result = response.into_inner();

        // Try inline first, then fetch from CAS
        let stdout = if !action_result.stdout_raw.is_empty() {
            Some(String::from_utf8_lossy(&action_result.stdout_raw).to_string())
        } else if let Some(digest) = &action_result.stdout_digest {
            let blobs = self.download_blobs(vec![digest.clone()]).await?;
            blobs
                .get(&digest.hash)
                .map(|b| String::from_utf8_lossy(b).to_string())
        } else {
            None
        };

        let stderr = if !action_result.stderr_raw.is_empty() {
            Some(String::from_utf8_lossy(&action_result.stderr_raw).to_string())
        } else if let Some(digest) = &action_result.stderr_digest {
            let blobs = self.download_blobs(vec![digest.clone()]).await?;
            blobs
                .get(&digest.hash)
                .map(|b| String::from_utf8_lossy(b).to_string())
        } else {
            None
        };

        Ok((stdout, stderr))
    }

    fn name(&self) -> &'static str {
        "remote"
    }

    async fn health_check(&self) -> BackendResult<()> {
        // Try to connect
        let _channel = self.get_channel().await?;
        Ok(())
    }
}

/// Check if a gRPC error is retryable
fn is_retryable(status: &tonic::Status) -> bool {
    matches!(
        status.code(),
        tonic::Code::Unavailable
            | tonic::Code::ResourceExhausted
            | tonic::Code::Aborted
            | tonic::Code::Internal
            | tonic::Code::Unknown
    )
}

// Silence unused warning for debug impl
impl std::fmt::Debug for RemoteCacheBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteCacheBackend")
            .field("config", &self.config)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_from_env_empty() {
        temp_env::with_var_unset("CUENV_REMOTE_CACHE_URL", || {
            let config = RemoteCacheConfig::from_env();
            assert!(config.is_none());
        });
    }

    #[test]
    fn test_to_bazel_digest() {
        let digest = RemoteCacheBackend::to_bazel_digest("sha256:abc123");
        assert_eq!(digest.hash, "abc123");

        let digest2 = RemoteCacheBackend::to_bazel_digest("def456");
        assert_eq!(digest2.hash, "def456");
    }

    #[test]
    fn test_compute_digest() {
        let data = b"hello world";
        let digest = RemoteCacheBackend::compute_digest(data);
        assert!(!digest.hash.is_empty());
        assert_eq!(digest.size_bytes, 11);
    }

    #[test]
    fn test_is_retryable() {
        assert!(is_retryable(&tonic::Status::unavailable("test")));
        assert!(is_retryable(&tonic::Status::resource_exhausted("test")));
        assert!(!is_retryable(&tonic::Status::not_found("test")));
        assert!(!is_retryable(&tonic::Status::permission_denied("test")));
    }
}
