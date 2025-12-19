pub mod error;
pub mod merkle;
pub mod client;
pub mod mapper;
pub mod retry;

pub mod reapi {
    pub mod build {
        pub mod bazel {
            pub mod remote {
                pub mod execution {
                    pub mod v2 {
                        tonic::include_proto!("build.bazel.remote.execution.v2");
                    }
                }
            }
            pub mod semver {
                tonic::include_proto!("build.bazel.semver");
            }
        }
    }
    pub mod google {
        pub mod longrunning {
            tonic::include_proto!("google.longrunning");
        }
        pub mod rpc {
            tonic::include_proto!("google.rpc");
        }
        pub mod api {
            tonic::include_proto!("google.api");
        }
    }
}

pub use error::RemoteError;

use cuenv_core::tasks::{Task, TaskResult, TaskBackend};
use cuenv_core::environment::Environment;
use cuenv_core::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tonic::transport::Channel;
use prost::Message;
use futures::StreamExt;

// Aliases for convenience
use reapi::build::bazel::remote::execution::v2 as remote;
use reapi::google::longrunning as longrunning;

pub struct RemoteBackend {
    _project_root: PathBuf,
    cas: Arc<dyn client::cas::Cas>,
    action_cache: Arc<dyn client::action_cache::ActionCache>,
    execution: Arc<dyn client::execution::Execution>,
    _instance_name: String,
}

impl RemoteBackend {
    pub fn new(
        project_root: PathBuf,
        channel: Channel,
        instance_name: String,
    ) -> Self {
        Self {
            _project_root: project_root,
            cas: Arc::new(client::cas::CasClient::new(channel.clone(), instance_name.clone())),
            action_cache: Arc::new(client::action_cache::ActionCacheClient::new(channel.clone(), instance_name.clone())),
            execution: Arc::new(client::execution::ExecutionClient::new(channel, instance_name.clone())),
            _instance_name: instance_name,
        }
    }
}

#[async_trait]
impl TaskBackend for RemoteBackend {
    async fn execute(
        &self,
        name: &str,
        task: &Task,
        environment: &Environment,
        project_root: &Path,
        _capture_output: bool,
    ) -> Result<TaskResult> {
        tracing::info!(task = %name, backend = "remote", "Executing task remotely");

        // 1. Resolve inputs
        let resolver = cuenv_core::tasks::io::InputResolver::new(project_root);
        let input_patterns = task.collect_all_inputs_with_prefix(None);
        let resolved_inputs = resolver.resolve(&input_patterns)?;

        // 2. Build Merkle tree
        let merkle_tree = merkle::directory::MerkleTree::from_inputs(&resolved_inputs)
            .map_err(|e| cuenv_core::Error::configuration(format!("Failed to build Merkle tree: {}", e)))?;

        // 3. Map Task to REAPI Command
        let command = mapper::command::CommandMapper::map_task(task, &environment.vars)
            .map_err(|e| cuenv_core::Error::configuration(format!("Failed to map task: {}", e)))?;
        
        let mut command_buf = Vec::new();
        command.encode(&mut command_buf).expect("failed to encode command");
        let command_digest = merkle::digest::Digest::from_content(&command_buf);

        // 4. Build Action
        let action = mapper::action::ActionBuilder::build_action(
            command_digest.clone(),
            merkle_tree.root_digest.clone(),
            None,
            false,
        ).map_err(|e| cuenv_core::Error::configuration(format!("Failed to build action: {}", e)))?;

        let mut action_buf = Vec::new();
        action.encode(&mut action_buf).expect("failed to encode action");
        let action_digest = merkle::digest::Digest::from_content(&action_buf);

        // 5. Check ActionCache
        if let Some(action_result) = self.action_cache.get_action_result(action_digest.clone()).await.ok().flatten() {
             tracing::info!(task = %name, "Action cache hit");
             // TODO: download outputs
             return Ok(mapper::result::ResultMapper::map_result(name, action_result));
        }

        // 6. Miss: Upload missing blobs
        let mut blobs_to_upload = Vec::new();
        blobs_to_upload.push((command_digest.clone(), command_buf));
        blobs_to_upload.push((action_digest.clone(), action_buf));
        
        for (digest, dir) in &merkle_tree.directories {
            let mut buf = Vec::new();
            dir.to_proto().encode(&mut buf).expect("failed to encode dir");
            blobs_to_upload.push((digest.clone(), buf));
        }
        
        for input in &resolved_inputs.files {
            let data = std::fs::read(&input.source_path).map_err(|e| cuenv_core::Error::Io { 
                source: e, 
                path: Some(input.source_path.clone().into()), 
                operation: "read".into() 
            })?;
            blobs_to_upload.push((merkle::digest::Digest::new(input.sha256.clone(), input.size as i64), data));
        }

        let missing = self.cas.find_missing_blobs(blobs_to_upload.iter().map(|(d, _)| d.clone()).collect()).await
            .map_err(|e| cuenv_core::Error::configuration(format!("CAS error: {}", e)))?;
        
        let to_upload: Vec<_> = blobs_to_upload.into_iter().filter(|(d, _)| missing.contains(d)).collect();
        if !to_upload.is_empty() {
            self.cas.batch_upload_blobs(to_upload).await
                .map_err(|e| cuenv_core::Error::configuration(format!("Upload error: {}", e)))?;
        }

        // 7. Execute
        let mut stream = self.execution.execute(action_digest.clone(), false).await
            .map_err(|e| cuenv_core::Error::configuration(format!("Execution error: {}", e)))?;
        
        while let Some(op_res) = stream.next().await {
            let op = op_res.map_err(|e| cuenv_core::Error::configuration(format!("Stream error: {}", e)))?;
            if op.done {
                if let Some(result) = op.result {
                    match result {
                        longrunning::operation::Result::Response(response) => {
                            let execute_response = remote::ExecuteResponse::decode(response.value.as_slice())
                                .map_err(|e| cuenv_core::Error::configuration(format!("Failed to decode response: {}", e)))?;
                            
                            if let Some(result) = execute_response.result {
                                // TODO: download outputs
                                return Ok(mapper::result::ResultMapper::map_result(name, result));
                            }
                        }
                        longrunning::operation::Result::Error(status) => {
                            return Err(cuenv_core::Error::configuration(format!("Remote execution error: {}", status.message)));
                        }
                    }
                }
                return Err(cuenv_core::Error::configuration("Operation finished without result"));
            }
        }

        Err(cuenv_core::Error::configuration("Execution stream ended prematurely"))
    }

    fn name(&self) -> &'static str {
        "remote"
    }
}
