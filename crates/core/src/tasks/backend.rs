//! Task backend abstraction for different execution environments
//!
//! This module provides a pluggable backend system for task execution.
//! The default backend is `Host`, which runs tasks directly on the host machine.
//! For Dagger container execution, use the `cuenv-dagger` crate.

use super::{Task, TaskResult};
use crate::config::BackendConfig;
use crate::environment::Environment;
use crate::{Error, Result};
use async_trait::async_trait;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;

/// Trait for task execution backends
#[async_trait]
pub trait TaskBackend: Send + Sync {
    /// Execute a single task and return the result
    async fn execute(
        &self,
        name: &str,
        task: &Task,
        environment: &Environment,
        project_root: &Path,
        capture_output: bool,
    ) -> Result<TaskResult>;

    /// Get the name of the backend
    fn name(&self) -> &'static str;
}

/// Host backend - executes tasks directly on the host machine
pub struct HostBackend;

impl Default for HostBackend {
    fn default() -> Self {
        Self
    }
}

impl HostBackend {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl TaskBackend for HostBackend {
    async fn execute(
        &self,
        name: &str,
        task: &Task,
        environment: &Environment,
        project_root: &Path,
        capture_output: bool,
    ) -> Result<TaskResult> {
        tracing::info!(
            task = %name,
            backend = "host",
            "Executing task on host"
        );

        // Resolve command path using the environment's PATH
        let resolved_command = environment.resolve_command(&task.command);

        // Build command
        let mut cmd = if let Some(shell) = &task.shell {
            let mut c = Command::new(shell.command.as_deref().unwrap_or("bash"));
            if let Some(flag) = &shell.flag {
                c.arg(flag);
            } else {
                c.arg("-c");
            }
            // Append the task command string to the shell invocation
            c.arg(&task.command);
            c
        } else {
            let mut c = Command::new(resolved_command);
            c.args(&task.args);
            c
        };

        // Set working directory
        cmd.current_dir(project_root);

        // Set environment variables
        cmd.env_clear();
        for (k, v) in &environment.vars {
            cmd.env(k, v);
        }

        // Execute - always capture output for consistent behavior
        if capture_output {
            let output = cmd
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .map_err(|e| Error::Io {
                    source: e,
                    path: None,
                    operation: format!("spawn task {}", name),
                })?;

            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);
            let success = output.status.success();

            if !success {
                tracing::warn!(task = %name, exit = exit_code, "Task failed");
            }

            Ok(TaskResult {
                name: name.to_string(),
                exit_code: Some(exit_code),
                stdout,
                stderr,
                success,
            })
        } else {
            // Stream output directly to terminal (interactive mode)
            let status = cmd
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .await
                .map_err(|e| Error::Io {
                    source: e,
                    path: None,
                    operation: format!("spawn task {}", name),
                })?;

            let exit_code = status.code().unwrap_or(-1);
            let success = status.success();

            if !success {
                tracing::warn!(task = %name, exit = exit_code, "Task failed");
            }

            Ok(TaskResult {
                name: name.to_string(),
                exit_code: Some(exit_code),
                stdout: String::new(), // Output went to terminal
                stderr: String::new(),
                success,
            })
        }
    }

    fn name(&self) -> &'static str {
        "host"
    }
}

/// Type alias for a backend factory function
pub type BackendFactory = fn(Option<&BackendConfig>, std::path::PathBuf) -> Arc<dyn TaskBackend>;

/// Create a backend based on configuration.
///
/// This function only handles the `host` backend. For `dagger` or `remote` backend support,
/// use `create_backend_with_factory` and provide the appropriate factories.
pub fn create_backend(
    config: Option<&BackendConfig>,
    project_root: std::path::PathBuf,
    cli_backend: Option<&str>,
) -> Arc<dyn TaskBackend> {
    create_backend_with_factory(config, project_root, cli_backend, None, None)
}

/// Create a backend with optional factories for non-host backends.
pub fn create_backend_with_factory(
    config: Option<&BackendConfig>,
    project_root: std::path::PathBuf,
    cli_backend: Option<&str>,
    dagger_factory: Option<BackendFactory>,
    remote_factory: Option<BackendFactory>,
) -> Arc<dyn TaskBackend> {
    // CLI override takes precedence, then config, then default to host
    let backend_type = if let Some(b) = cli_backend {
        b.to_string()
    } else if let Some(c) = config {
        c.backend_type.clone()
    } else {
        "host".to_string()
    };

    match backend_type.as_str() {
        "dagger" => {
            if let Some(factory) = dagger_factory {
                factory(config, project_root)
            } else {
                tracing::error!(
                    "Dagger backend requested but not available. \
                     Add cuenv-dagger dependency to enable it. \
                     Falling back to host backend."
                );
                Arc::new(HostBackend::new())
            }
        }
        "remote" => {
            if let Some(factory) = remote_factory {
                factory(config, project_root)
            } else {
                tracing::error!(
                    "Remote backend requested but not available. \
                     Add cuenv-remote dependency to enable it. \
                     Falling back to host backend."
                );
                Arc::new(HostBackend::new())
            }
        }
        _ => Arc::new(HostBackend::new()),
    }
}

/// Check if the dagger backend should be used based on configuration
pub fn should_use_dagger(config: Option<&BackendConfig>, cli_backend: Option<&str>) -> bool {
    let backend_type = if let Some(b) = cli_backend {
        b
    } else if let Some(c) = config {
        &c.backend_type
    } else {
        "host"
    };

    backend_type == "dagger"
}
