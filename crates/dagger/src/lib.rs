//! Dagger backend for cuenv task execution
//!
//! This crate provides the `DaggerBackend` implementation that executes tasks
//! inside containers using the Dagger SDK.

use async_trait::async_trait;
use cuenv_core::config::BackendConfig;
use cuenv_core::environment::Environment;
use cuenv_core::tasks::{Task, TaskBackend, TaskResult};
use cuenv_core::{Error, Result};
use dagger_sdk::{Config, ContainerId, connect_opts};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

type DaggerReport = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Dagger backend - executes tasks inside containers using Dagger
pub struct DaggerBackend {
    default_image: Option<String>,
    project_root: std::path::PathBuf,
    container_cache: Arc<Mutex<HashMap<String, ContainerId>>>,
}

impl DaggerBackend {
    pub fn new(default_image: Option<String>, project_root: std::path::PathBuf) -> Self {
        Self {
            default_image,
            project_root,
            container_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get the container cache for storing/retrieving container IDs
    pub fn container_cache(&self) -> &Arc<Mutex<HashMap<String, ContainerId>>> {
        &self.container_cache
    }
}

#[async_trait]
impl TaskBackend for DaggerBackend {
    async fn execute(
        &self,
        name: &str,
        task: &Task,
        env: &Environment,
        _project_root: &Path,
        capture_output: bool,
    ) -> Result<TaskResult> {
        let dagger_config = task.dagger.as_ref();

        // Determine if we're using container chaining (from) or a base image
        let from_task = dagger_config.and_then(|d| d.from.clone());
        let image = dagger_config
            .and_then(|d| d.image.clone())
            .or_else(|| self.default_image.clone());

        // Validate: must have either 'from' or 'image'
        if from_task.is_none() && image.is_none() {
            return Err(Error::configuration(
                "Dagger backend requires either 'image' or 'from' (task reference). \
                 Set tasks.<name>.dagger.image, tasks.<name>.dagger.from, or config.backend.options.image"
                    .to_string(),
            ));
        }

        let command: Vec<String> = std::iter::once(task.command.clone())
            .chain(task.args.clone())
            .collect();

        if command.is_empty() {
            return Err(Error::configuration(
                "Dagger task requires a command to execute".to_string(),
            ));
        }

        // Resolve secrets before entering the Dagger closure
        let mut resolved_secrets: Vec<(String, Option<String>, Option<String>, String)> =
            Vec::new();
        if let Some(secrets) = dagger_config.and_then(|d| d.secrets.as_ref()) {
            for secret in secrets {
                let plaintext = secret.resolver.resolve().await?;
                resolved_secrets.push((
                    secret.name.clone(),
                    secret.path.clone(),
                    secret.env_var.clone(),
                    plaintext,
                ));
            }
        }

        // Get cache mounts
        let cache_mounts: Vec<(String, String)> = dagger_config
            .and_then(|d| d.cache.as_ref())
            .map(|caches| {
                caches
                    .iter()
                    .map(|c| (c.path.clone(), c.name.clone()))
                    .collect()
            })
            .unwrap_or_default();

        // Get container ID from cache if using 'from'
        let cached_container_id = if let Some(ref from_name) = from_task {
            let cache = self.container_cache.lock().map_err(|_| {
                Error::configuration("Failed to acquire container cache lock".to_string())
            })?;
            cache.get(from_name).cloned()
        } else {
            None
        };

        // Validate that referenced task exists in cache when using 'from'
        if from_task.is_some() && cached_container_id.is_none() {
            return Err(Error::configuration(format!(
                "Task '{}' references container from task '{}', but no container was found. \
                 Ensure the referenced task runs first (use dependsOn).",
                name,
                from_task.as_ref().expect("checked is_some above")
            )));
        }

        let env_map = env.vars.clone();
        let project_root = self.project_root.clone();
        let task_name = name.to_string();
        let task_name_for_cache = task_name.clone();
        let container_cache = self.container_cache.clone();

        // Result store: (exit_code, stdout, stderr, container_id)
        type ResultType = (i32, String, String, Option<ContainerId>);
        let result_store: Arc<Mutex<Option<std::result::Result<ResultType, DaggerReport>>>> =
            Arc::new(Mutex::new(None));
        let result_store_clone = result_store.clone();

        let cfg = Config::default();

        connect_opts(cfg, move |client| {
            let project_root = project_root.clone();
            let image = image.clone();
            let command = command.clone();
            let env_map = env_map.clone();
            let result_store = result_store_clone.clone();
            let resolved_secrets = resolved_secrets.clone();
            let cache_mounts = cache_mounts.clone();
            let cached_container_id = cached_container_id.clone();
            let task_name_inner = task_name.clone();

            async move {
                let host_dir = client
                    .host()
                    .directory(project_root.to_string_lossy().to_string());

                // Create base container: either from cached container or from image
                // IMPORTANT: Only mount host directory when starting fresh (not chaining)
                // to preserve files created in /workspace by previous tasks
                let mut container = if let Some(container_id) = cached_container_id {
                    // Continue from previous task's container
                    // DO NOT re-mount /workspace - it would overwrite files from previous tasks
                    client
                        .load_container_from_id(container_id)
                        .with_workdir("/workspace")
                } else if let Some(img) = image {
                    // Start from base image - mount host directory at /workspace
                    client
                        .container()
                        .from(img)
                        .with_mounted_directory("/workspace", host_dir)
                        .with_workdir("/workspace")
                } else {
                    // This shouldn't happen due to earlier validation
                    if let Ok(mut guard) = result_store.lock() {
                        *guard = Some(Err("No image or container reference provided".into()));
                    }
                    return Ok(());
                };

                // Mount cache volumes
                for (path, cache_name) in &cache_mounts {
                    let cache_vol = client.cache_volume(cache_name);
                    container = container.with_mounted_cache(path, cache_vol);
                }

                // Set up secrets
                for (secret_name, path, env_var, plaintext) in &resolved_secrets {
                    let dagger_secret = client.set_secret(secret_name, plaintext);

                    if let Some(file_path) = path {
                        container = container.with_mounted_secret(file_path, dagger_secret.clone());
                    }
                    if let Some(var_name) = env_var {
                        container = container.with_secret_variable(var_name, dagger_secret);
                    }
                }

                // Set environment variables
                for (k, v) in env_map {
                    container = container.with_env_variable(k, v);
                }

                // Execute command
                let exec = container.with_exec(command);

                // Get results
                let stdout_res = exec.stdout().await;
                let stderr_res = exec.stderr().await;
                let exit_code_res = exec.exit_code().await;
                let container_id_res = exec.id().await;

                let res = match (stdout_res, stderr_res, exit_code_res, container_id_res) {
                    (Ok(stdout), Ok(stderr), Ok(exit_code), Ok(container_id)) => {
                        Ok((exit_code as i32, stdout, stderr, Some(container_id)))
                    }
                    (Ok(stdout), Ok(stderr), Ok(exit_code), Err(_)) => {
                        // Container ID fetch failed but execution succeeded
                        tracing::warn!(
                            task = %task_name_inner,
                            "Failed to get container ID for caching"
                        );
                        Ok((exit_code as i32, stdout, stderr, None))
                    }
                    (Err(e), _, _, _) => Err(e.into()),
                    (_, Err(e), _, _) => Err(e.into()),
                    (_, _, Err(e), _) => Err(e.into()),
                };

                if let Ok(mut guard) = result_store.lock() {
                    *guard = Some(res);
                }
                Ok(())
            }
        })
        .await
        .map_err(|err| Error::execution(format!("Dagger backend failed: {err}")))?;

        // Extract result
        let mut guard = result_store
            .lock()
            .map_err(|_| Error::execution("Failed to acquire lock on task result".to_string()))?;

        let inner_result = guard
            .take()
            .ok_or_else(|| Error::execution("Task completed but produced no result".to_string()))?;

        let (exit_code, stdout, stderr, container_id) = inner_result
            .map_err(|e: DaggerReport| Error::execution(format!("Dagger execution failed: {e}")))?;

        // Cache the container ID for potential use by subsequent tasks
        if let Some(cid) = container_id
            && let Ok(mut cache) = container_cache.lock()
        {
            cache.insert(task_name_for_cache.clone(), cid);
        }

        // Print output if not capturing
        if !capture_output {
            if !stdout.is_empty() {
                print!("{}", stdout);
            }
            if !stderr.is_empty() {
                eprint!("{}", stderr);
            }
        }

        Ok(TaskResult {
            name: task_name_for_cache,
            exit_code: Some(exit_code),
            stdout: if capture_output {
                stdout
            } else {
                String::new()
            },
            stderr: if capture_output {
                stderr
            } else {
                String::new()
            },
            success: exit_code == 0,
        })
    }

    fn name(&self) -> &'static str {
        "dagger"
    }
}

/// Create a Dagger backend from configuration
pub fn create_dagger_backend(
    config: Option<&BackendConfig>,
    project_root: std::path::PathBuf,
) -> Arc<dyn TaskBackend> {
    let image = config
        .and_then(|c| c.options.as_ref())
        .and_then(|o| o.image.clone());
    Arc::new(DaggerBackend::new(image, project_root))
}
