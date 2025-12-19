//! REAPI remote backend implementation

use crate::client::{ActionCacheClient, ByteStreamClient, CasClient, ExecutionClient, GrpcChannel};
use crate::config::RemoteConfig;
use crate::error::{RemoteError, Result};
use crate::mapper::{ActionBuilder, CommandMapper, ResultMapper};
use crate::merkle::{Digest, DirectoryBuilder};
use crate::nix::{self, NixInputs};
use async_trait::async_trait;
use cuenv_core::environment::Environment;
use cuenv_core::tasks::io::{InputResolver, ResolvedInputFile};
use cuenv_core::tasks::{Input, Task, TaskBackend, TaskResult};
use std::collections::{HashMap, HashSet};
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
    ///
    /// Nix packages are read from `config.nix_packages`, which should be
    /// populated before calling this function (e.g., from `project.packages.nix`).
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
                    bytestream: ByteStreamClient::from_channel(&channel, self.config.clone()),
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
        let resolved = resolver
            .resolve(&patterns)
            .map_err(|e| RemoteError::merkle_error(format!("Failed to resolve inputs: {}", e)))?;

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

    /// Build Merkle tree from task inputs plus Nix toolchain files
    fn build_input_tree_with_nix(
        &self,
        task: &Task,
        nix_inputs: &NixInputs,
    ) -> Result<(DirectoryBuilder, HashMap<Digest, PathBuf>)> {
        // Start with regular task inputs
        let (mut builder, mut file_sources) = self.build_input_tree(task)?;

        // Add Nix toolchain files if present
        if !nix_inputs.is_empty() {
            debug!(
                nix_files = nix_inputs.files.len(),
                "Adding Nix toolchain files to input tree"
            );

            for nix_file in &nix_inputs.files {
                if nix_file.is_symlink {
                    // Add symlink to the directory tree
                    if let Some(target) = &nix_file.symlink_target {
                        builder.add_symlink(&nix_file.relative_path, target)?;
                    }
                } else {
                    // Add regular file
                    file_sources.insert(nix_file.digest.clone(), nix_file.store_path.clone());
                    builder.add_file_with_permissions(
                        &nix_file.relative_path,
                        nix_file.digest.clone(),
                        nix_file.is_executable,
                    )?;
                }
            }
        }

        Ok((builder, file_sources))
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

        // 2. Detect and prepare Nix toolchain inputs
        // Priority:
        // 1. Explicit packages (packages: nix in env.cue) - fetched for target platform
        // 2. Environment-based detection - fallback for legacy hook approach
        let nix_inputs = if !self.config.nix_packages.is_empty() {
            // Use explicit package list from packages: nix
            let target = self
                .config
                .target_platform
                .as_deref()
                .unwrap_or("x86_64-linux");
            info!(
                task = %name,
                target = %target,
                package_count = self.config.nix_packages.len(),
                "Using explicit Nix packages"
            );
            nix::prepare_inputs_from_packages(&self.config.nix_packages, target).await?
        } else if let Some(ref target) = self.config.target_platform {
            // Legacy: fetch hardcoded packages for target platform
            info!(
                task = %name,
                target = %target,
                "Using cross-platform Nix closure (legacy)"
            );
            nix::prepare_inputs_for_platform(target, &environment.vars).await?
        } else {
            // Legacy: extract paths from environment
            nix::prepare_inputs_parallel(&environment.vars).await?
        };

        // 3. Build remote environment with Nix package paths
        let remote_env = if nix_inputs.is_empty() {
            environment.clone()
        } else {
            info!(
                nix_files = nix_inputs.files.len(),
                nix_size_mb = nix_inputs.total_size / 1_000_000,
                package_roots = nix_inputs.package_roots.len(),
                "Including Nix toolchain in remote execution"
            );

            // Start with rewritten environment paths
            let store_paths: HashSet<PathBuf> = nix_inputs.path_mapping.keys().cloned().collect();
            let mut rewritten_vars = nix::rewrite_paths(&environment.vars, &store_paths);

            // Build PATH from package roots (for packages: nix approach)
            if !nix_inputs.package_roots.is_empty() {
                let nix_path = nix_inputs.build_path();
                debug!(nix_path = %nix_path, "Constructed PATH from package roots");

                // Prepend Nix PATH to existing PATH (or set if none exists)
                let final_path = match rewritten_vars.get("PATH") {
                    Some(existing) if !existing.is_empty() => {
                        format!("{}:{}", nix_path, existing)
                    }
                    _ => nix_path,
                };
                rewritten_vars.insert("PATH".to_string(), final_path);

                // Set CC=gcc for Rust compilation - Nix's gcc doesn't provide 'cc' symlink
                // which is what Rust's linker detection expects by default
                if !rewritten_vars.contains_key("CC") {
                    rewritten_vars.insert("CC".to_string(), "gcc".to_string());
                }
            }

            Environment::from_map(rewritten_vars)
        };

        // 4. Wrap task if using Nix packages (need /nix symlink for binaries)
        let wrapped_task = if !nix_inputs.package_roots.is_empty() {
            info!(
                task = %name,
                "Wrapping command with /nix symlink setup for Nix binaries"
            );
            self.wrap_task_for_nix(task)
        } else {
            task.clone()
        };

        // 5. Map Task to REAPI Command with rewritten environment
        let mapped_command =
            CommandMapper::map_task(&wrapped_task, &remote_env, &self.config.secrets)?;

        // 6. Build Merkle tree from task inputs + Nix toolchain
        let (dir_builder, file_sources) = self.build_input_tree_with_nix(task, &nix_inputs)?;
        let input_tree = dir_builder.build()?;

        // 6. Build Action with input root + command digest + timeout
        let mapped_action = ActionBuilder::build_action(
            &mapped_command,
            &input_tree.root_digest,
            Some(self.config.timeout_secs),
        )?;

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
    ///
    /// Uses ByteStream for large files to avoid memory issues with big Nix closures.
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

        // Add files (we'll upload them via CAS/ByteStream based on size)
        let file_digests: Vec<Digest> = file_sources.keys().cloned().collect();
        all_digests.extend(file_digests);

        // Find which blobs are missing
        let missing = clients.cas.find_missing_blobs(&all_digests).await?;
        if missing.is_empty() {
            debug!("All blobs already in CAS");
            return Ok(());
        }

        info!(
            missing_count = missing.len(),
            "Uploading missing blobs to CAS"
        );

        // Separate missing blobs into in-memory blobs and file sources
        let mut blobs_to_upload: Vec<(Digest, Vec<u8>)> = Vec::new();
        let mut missing_file_sources: HashMap<Digest, PathBuf> = HashMap::new();

        for digest in &missing {
            // Check if it's a pre-computed blob (command, action, directory)
            if let Some(bytes) = digest_to_bytes.get(&digest.hash) {
                blobs_to_upload.push((digest.clone(), bytes.clone()));
            } else if let Some(path) = file_sources.get(digest) {
                // It's a file - add to file sources for upload
                missing_file_sources.insert(digest.clone(), path.clone());
            } else {
                // This is an invariant violation - fail immediately with clear error
                return Err(RemoteError::merkle_error(format!(
                    "Internal error: missing blob {} has no known source. This indicates a bug in input tree construction.",
                    digest.hash
                )));
            }
        }

        // Upload using CAS + ByteStream (large files stream from disk)
        clients
            .cas
            .upload_with_bytestream(&clients.bytestream, &blobs_to_upload, &missing_file_sources)
            .await?;

        debug!(
            uploaded_count = missing.len(),
            "Uploaded missing blobs"
        );
        Ok(())
    }

    /// Wrap a task's command to create /nix symlink for Nix binaries
    ///
    /// Nix binaries have hardcoded absolute paths like `/nix/store/xxx/lib/...`.
    /// On remote workers, our files are at `<working_dir>/nix/store/...`.
    /// This wrapper creates a symlink `/nix -> $PWD/nix` before running the command.
    fn wrap_task_for_nix(&self, task: &Task) -> Task {
        // Build the wrapper script
        // Note: We try both `sudo ln` and regular `ln` - BuildBuddy runners
        // often have passwordless sudo or run as root
        let original_command = if let Some(ref shell) = task.shell {
            // Shell mode - the command is already a script
            task.command.clone()
        } else {
            // Direct mode - build command with args
            let mut cmd = task.command.clone();
            for arg in &task.args {
                cmd.push(' ');
                // Simple quoting - escape single quotes
                if arg.contains(' ') || arg.contains('\'') || arg.contains('"') {
                    cmd.push('\'');
                    cmd.push_str(&arg.replace('\'', "'\"'\"'"));
                    cmd.push('\'');
                } else {
                    cmd.push_str(arg);
                }
            }
            cmd
        };

        // Wrapper script that:
        // 1. Creates /nix symlink pointing to $PWD/nix (using system tools, not Nix)
        // 2. Runs the original command
        let wrapper_script = format!(
            r#"set -e
# Create /nix symlink for Nix store access
# IMPORTANT: Use absolute paths to system tools, not Nix tools from PATH
if [ -d "$PWD/nix" ] && [ ! -e "/nix" ]; then
    /bin/ln -sf "$PWD/nix" /nix 2>/dev/null || /usr/bin/sudo /bin/ln -sf "$PWD/nix" /nix 2>/dev/null || true
fi
# Run original command
{}"#,
            original_command
        );

        debug!(
            original = %original_command,
            "Wrapped command with /nix symlink setup"
        );

        Task {
            command: wrapper_script,
            shell: Some(cuenv_core::tasks::Shell {
                // Use absolute path to system bash, not Nix bash from PATH
                command: Some("/bin/bash".to_string()),
                flag: Some("-c".to_string()),
            }),
            args: Vec::new(), // Args are now in the script
            ..task.clone()
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
    bytestream: ByteStreamClient,
    action_cache: ActionCacheClient,
    execution: ExecutionClient,
}
