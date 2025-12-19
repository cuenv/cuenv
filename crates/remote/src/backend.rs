//! REAPI remote backend implementation

use crate::client::{ActionCacheClient, CasClient, CapabilitiesClient, ExecutionClient};
use crate::config::RemoteConfig;
use crate::error::{RemoteError, Result};
use crate::mapper::{ActionBuilder, CommandMapper, ResultMapper};
use crate::merkle::DirectoryBuilder;
use async_trait::async_trait;
use cuenv_core::environment::Environment;
use cuenv_core::tasks::{Task, TaskBackend, TaskResult};
use std::path::{Path, PathBuf};
use tracing::{debug, info};

/// Remote backend that executes tasks via REAPI
pub struct RemoteBackend {
    config: RemoteConfig,
    project_root: PathBuf,
}

impl RemoteBackend {
    /// Create a new remote backend
    pub fn new(config: RemoteConfig, project_root: PathBuf) -> Self {
        Self {
            config,
            project_root,
        }
    }

    /// Initialize gRPC clients
    ///
    /// Phase 2: Implement actual gRPC client initialization with tonic
    fn _create_clients(&self) -> Result<RemoteClients> {
        // TODO: In Phase 2, create actual gRPC channels and clients

        Ok(RemoteClients {
            cas: CasClient::new(self.config.clone()),
            action_cache: ActionCacheClient::new(self.config.clone()),
            execution: ExecutionClient::new(self.config.clone()),
            capabilities: CapabilitiesClient::new(self.config.clone()),
        })
    }

    /// Build Merkle tree from task inputs
    ///
    /// Phase 3: Implement proper Merkle tree construction
    fn _build_input_tree(&self, _task: &Task) -> Result<DirectoryBuilder> {
        let mut builder = DirectoryBuilder::new(&self.project_root);

        // TODO: Phase 3 - scan task.inputs and build tree
        // For each input:
        //   1. Compute file digest
        //   2. Add to DirectoryBuilder
        // Then call builder.build() to get root digest

        Ok(builder)
    }

    /// Execute a task remotely (full implementation in Phase 4)
    async fn _execute_remote(
        &self,
        name: &str,
        task: &Task,
        environment: &Environment,
    ) -> Result<TaskResult> {
        info!(task = %name, backend = "remote", "Starting remote execution");

        // Phase 4 implementation outline:

        // 1. Resolve secrets on coordinator
        // let resolved_env = resolve_secrets(environment).await?;

        // 2. Map Task to REAPI Command
        let (_command, _secrets_headers) =
            CommandMapper::map_task(task, environment, &self.config.secrets)?;

        // 3. Build Merkle tree from inputs
        // let input_tree = self.build_input_tree(task)?;
        // let input_root_digest = input_tree.build()?;

        // 4. Build Action
        // let (action, action_digest) = ActionBuilder::build_action(
        //     &command,
        //     input_root_digest,
        //     Some(self.config.timeout_secs),
        // )?;

        // 5. Check ActionCache for hit
        // if self.config.remote_cache {
        //     if let Some(cached_result) = clients.action_cache.get_action_result(&action_digest).await? {
        //         info!(task = %name, "Cache hit!");
        //         return ResultMapper::map_result(name, cached_result, true).await;
        //     }
        // }

        // 6. Upload missing blobs to CAS
        // let all_digests = collect_all_digests(&input_tree, &command);
        // let missing = clients.cas.find_missing_blobs(&all_digests).await?;
        // for digest in missing {
        //     let data = read_file_for_digest(&digest)?;
        //     clients.cas.upload_blob(&digest, data).await?;
        // }

        // 7. Execute remotely
        // let action_result = clients.execution.execute(&action_digest).await?;

        // 8. Update cache
        // if self.config.remote_cache {
        //     clients.action_cache.update_action_result(&action_digest, action_result.clone()).await?;
        // }

        // 9. Download outputs
        // let task_result = ResultMapper::map_result(name, action_result, true).await?;

        // For now, return error indicating not implemented
        Err(RemoteError::config_error(
            "Remote execution not yet fully implemented. \
             This is Phase 1-2 foundation. Full execution flow will be added in Phase 4.",
        ))
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

        // Attempt remote execution
        self._execute_remote(name, task, environment)
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
    #[allow(dead_code)]
    capabilities: CapabilitiesClient,
}
