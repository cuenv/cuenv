//! REAPI remote backend implementation

use crate::client::{ActionCacheClient, CasClient, ExecutionClient, GrpcChannel};
use crate::config::RemoteConfig;
use crate::error::{RemoteError, Result};
use crate::mapper::{ActionBuilder, CommandMapper, ResultMapper};
use crate::merkle::{Digest, DirectoryBuilder};
use async_trait::async_trait;
use cuenv_core::environment::Environment;
use cuenv_core::tasks::io::{InputResolver, ResolvedInputFile};
use cuenv_core::tasks::{Input, Task, TaskBackend, TaskResult};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tokio::sync::OnceCell;
use tracing::{debug, info, warn};

/// Remote backend that executes tasks via REAPI
pub struct RemoteBackend {
    config: RemoteConfig,
    project_root: PathBuf,
    /// Lazy-initialized gRPC clients
    clients: OnceCell<RemoteClients>,
}

impl RemoteBackend {
    /// Create a new remote backend
    pub fn new(config: RemoteConfig, project_root: PathBuf) -> Self {
        Self {
            config,
            project_root,
            clients: OnceCell::new(),
        }
    }

    /// Get or initialize gRPC clients (lazy initialization)
    async fn get_clients(&self) -> Result<&RemoteClients> {
        self.clients
            .get_or_try_init(|| async {
                debug!(endpoint = %self.config.endpoint, "Connecting to REAPI server");
                let channel = GrpcChannel::connect(&self.config).await?;

                Ok(RemoteClients {
                    cas: CasClient::from_channel(&channel, self.config.clone()),
                    action_cache: ActionCacheClient::from_channel(&channel, self.config.clone()),
                    execution: ExecutionClient::from_channel(&channel, self.config.clone()),
                })
            })
            .await
    }

    /// Build Merkle tree from task inputs
    ///
    /// Returns the directory tree and a map of file digests to their source paths
    fn build_input_tree(
        &self,
        task: &Task,
    ) -> Result<(DirectoryBuilder, HashMap<Digest, PathBuf>)> {
        let mut builder = DirectoryBuilder::new(&self.project_root);
        let mut file_sources: HashMap<Digest, PathBuf> = HashMap::new();

        // Extract path patterns from task inputs
        let patterns: Vec<String> = task
            .inputs
            .iter()
            .filter_map(|input| match input {
                Input::Path(p) => Some(p.clone()),
                // Project and Task references need special handling
                // (for now, skip them - they'd be resolved elsewhere)
                Input::Project(_) | Input::Task(_) => None,
            })
            .collect();

        if patterns.is_empty() {
            debug!(task = %task.command, "No input patterns, using empty input tree");
            return Ok((builder, file_sources));
        }

        // Resolve inputs to actual files
        let resolver = InputResolver::new(&self.project_root);
        let resolved = resolver.resolve(&patterns).map_err(|e| {
            RemoteError::merkle_error(format!("Failed to resolve inputs: {}", e))
        })?;

        debug!(
            file_count = resolved.files.len(),
            "Resolved input files for Merkle tree"
        );

        // Add resolved files to the directory builder
        for file in &resolved.files {
            let digest = self.file_to_digest(file)?;
            file_sources.insert(digest.clone(), file.source_path.clone());
            builder.add_file(&file.rel_path, digest)?;
        }

        Ok((builder, file_sources))
    }

    /// Convert a ResolvedInputFile to a Digest
    fn file_to_digest(&self, file: &ResolvedInputFile) -> Result<Digest> {
        // ResolvedInputFile already has SHA256 hash and size
        Digest::new(&file.sha256, file.size as i64)
    }

    /// Execute a task remotely (full implementation)
    async fn execute_remote(
        &self,
        name: &str,
        task: &Task,
        environment: &Environment,
    ) -> Result<TaskResult> {
        info!(task = %name, backend = "remote", endpoint = %self.config.endpoint, "Starting remote execution");

        // 1. Get or initialize clients
        let clients = self.get_clients().await?;

        // 2. Map Task to REAPI Command (resolves secrets on coordinator)
        let mapped_command = CommandMapper::map_task(task, environment, &self.config.secrets)?;
        debug!(
            task = %name,
            command_digest = %mapped_command.command_digest.hash,
            "Mapped task to REAPI Command"
        );

        // 3. Build Merkle tree from task inputs
        let (dir_builder, file_sources) = self.build_input_tree(task)?;
        let input_tree = dir_builder.build()?;
        debug!(
            task = %name,
            input_root = %input_tree.root_digest.hash,
            directory_count = input_tree.directories.len(),
            file_count = file_sources.len(),
            "Built input Merkle tree"
        );

        // 4. Build Action with input root + command digest + timeout
        let mapped_action = ActionBuilder::build_action(
            &mapped_command,
            &input_tree.root_digest,
            Some(self.config.timeout_secs),
        )?;
        debug!(
            task = %name,
            action_digest = %mapped_action.action_digest.hash,
            "Built REAPI Action"
        );

        // 5. Check ActionCache for hit
        if self.config.remote_cache {
            match clients
                .action_cache
                .get_action_result(&mapped_action.action_digest)
                .await
            {
                Ok(Some(cached_result)) => {
                    info!(task = %name, "Cache hit! Returning cached result");
                    return ResultMapper::map_result(name, cached_result, Some(&clients.cas)).await;
                }
                Ok(None) => {
                    debug!(task = %name, "Cache miss, proceeding with execution");
                }
                Err(e) => {
                    // Log but don't fail - cache miss is acceptable
                    warn!(task = %name, error = %e, "Failed to check action cache, proceeding with execution");
                }
            }
        }

        // 6. Upload missing blobs to CAS
        self.upload_missing_blobs(
            clients,
            &mapped_command.command_bytes,
            &mapped_command.command_digest,
            &mapped_action.action_bytes,
            &mapped_action.action_digest,
            &input_tree.directories,
            &file_sources,
        )
        .await?;

        // 7. Execute remotely
        info!(task = %name, "Submitting to remote execution service");
        let action_result = clients
            .execution
            .execute(&mapped_action.action_digest)
            .await?;

        // 8. Update ActionCache on success
        if self.config.remote_cache && action_result.exit_code == 0 {
            if let Err(e) = clients
                .action_cache
                .update_action_result(&mapped_action.action_digest, action_result.clone())
                .await
            {
                warn!(task = %name, error = %e, "Failed to update action cache");
            }
        }

        // 9. Map ActionResult to TaskResult
        ResultMapper::map_result(name, action_result, Some(&clients.cas)).await
    }

    /// Upload all blobs that are missing from CAS
    async fn upload_missing_blobs(
        &self,
        clients: &RemoteClients,
        command_bytes: &[u8],
        command_digest: &Digest,
        action_bytes: &[u8],
        action_digest: &Digest,
        directories: &[(Digest, Vec<u8>)],
        file_sources: &HashMap<Digest, PathBuf>,
    ) -> Result<()> {
        // Collect all digests we need to upload
        let mut all_digests: Vec<Digest> = Vec::new();
        let mut digest_to_bytes: HashMap<String, Vec<u8>> = HashMap::new();

        // Add command
        all_digests.push(command_digest.clone());
        digest_to_bytes.insert(command_digest.hash.clone(), command_bytes.to_vec());

        // Add action
        all_digests.push(action_digest.clone());
        digest_to_bytes.insert(action_digest.hash.clone(), action_bytes.to_vec());

        // Add directories
        for (digest, bytes) in directories {
            all_digests.push(digest.clone());
            digest_to_bytes.insert(digest.hash.clone(), bytes.clone());
        }

        // Add files (we'll read them when uploading)
        let file_digests: Vec<Digest> = file_sources.keys().cloned().collect();
        all_digests.extend(file_digests);

        // Find which blobs are missing
        let missing = clients.cas.find_missing_blobs(&all_digests).await?;
        if missing.is_empty() {
            debug!("All blobs already in CAS");
            return Ok(());
        }

        info!(missing_count = missing.len(), "Uploading missing blobs to CAS");

        // Prepare blobs for batch upload
        let mut blobs_to_upload: Vec<(Digest, Vec<u8>)> = Vec::new();

        for digest in &missing {
            // Check if it's a pre-computed blob (command, action, directory)
            if let Some(bytes) = digest_to_bytes.get(&digest.hash) {
                blobs_to_upload.push((digest.clone(), bytes.clone()));
            } else if let Some(path) = file_sources.get(digest) {
                // It's a file - read and upload
                let bytes = fs::read(path).map_err(|e| {
                    RemoteError::io_error(format!("read file {:?}", path), e)
                })?;
                blobs_to_upload.push((digest.clone(), bytes));
            } else {
                // This shouldn't happen
                warn!(hash = %digest.hash, "Missing blob with unknown source");
            }
        }

        // Upload all missing blobs
        clients.cas.batch_upload_blobs(&blobs_to_upload).await?;

        debug!(uploaded_count = blobs_to_upload.len(), "Uploaded missing blobs");
        Ok(())
    }
}

#[async_trait]
impl TaskBackend for RemoteBackend {
    async fn execute(
        &self,
        name: &str,
        task: &Task,
        environment: &Environment,
        _project_root: &Path,
        _capture_output: bool,
    ) -> cuenv_core::Result<TaskResult> {
        debug!(
            task = %name,
            backend = "remote",
            endpoint = %self.config.endpoint,
            "Executing task remotely"
        );

        // Execute remotely
        self.execute_remote(name, task, environment)
            .await
            .map_err(|e| cuenv_core::Error::execution(format!("Remote execution failed: {}", e)))
    }

    fn name(&self) -> &'static str {
        "remote"
    }
}

/// Container for REAPI gRPC clients
struct RemoteClients {
    cas: CasClient,
    action_cache: ActionCacheClient,
    execution: ExecutionClient,
}
