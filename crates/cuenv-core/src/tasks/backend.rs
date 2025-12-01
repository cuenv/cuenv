//! Task backend abstraction for different execution environments
//!
//! This module provides a pluggable backend system for task execution.
//! The default backend is `Host`, which runs tasks directly on the host machine.
//! The `Dagger` backend runs tasks inside containers using Dagger.

use super::{Task, TaskResult};
use crate::config::{BackendConfig, BackendOptions};
use crate::environment::Environment;
use crate::{Error, Result};
use async_trait::async_trait;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

#[cfg(feature = "dagger-backend")]
use dagger_sdk;

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
            let mut c = Command::new(&shell.command.as_ref().unwrap_or(&"bash".to_string()));
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

/// Dagger backend - executes tasks inside containers using Dagger
pub struct DaggerBackend {
    default_image: Option<String>,
    #[allow(dead_code)]
    project_root: std::path::PathBuf,
}

impl DaggerBackend {
    pub fn new(default_image: Option<String>, project_root: std::path::PathBuf) -> Self {
        Self {
            default_image,
            project_root,
        }
    }
}

#[cfg(feature = "dagger-backend")]
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
        let image = task
            .dagger
            .as_ref()
            .and_then(|dagger| dagger.image.clone())
            .or_else(|| self.default_image.clone())
            .ok_or_else(|| {
                Error::configuration(
                    "Dagger backend requires an image. Set tasks.<name>.dagger.image or config.backend.options.image"
                        .to_string(),
                )
            })?;

        let command: Vec<String> = std::iter::once(task.command.clone())
            .chain(task.args.clone())
            .collect();

        if command.is_empty() {
            return Err(Error::configuration(
                "Dagger task requires a command to execute".to_string(),
            ));
        }

        let env_map = env.vars.clone();
        let project_root = self.project_root.clone();
        let task_name = name.to_string();

        let (exit_code, stdout, stderr) = dagger_sdk::connect(|client| async move {
            let host_dir = client
                .host()
                .directory(project_root.to_string_lossy().to_string());

            let mut container = client
                .container()
                .from(image)
                .with_mounted_directory("/workspace", host_dir)
                .with_workdir("/workspace");

            for (k, v) in env_map {
                container = container.with_env_variable(k, v);
            }

            let exec = container.with_exec(command);
            
            // We need to handle stdout/stderr differently based on capture_output
            // The SDK's with_exec is lazy, so we force execution by requesting output
            
            let stdout_res = exec.stdout().await;
            let stderr_res = exec.stderr().await;
            let exit_code_res = exec.exit_code().await;

            match (stdout_res, stderr_res, exit_code_res) {
                (Ok(stdout), Ok(stderr), Ok(exit_code)) => Ok((exit_code as i32, stdout, stderr)),
                (Err(e), _, _) => Err(e),
                (_, Err(e), _) => Err(e),
                (_, _, Err(e)) => Err(e),
            }
        })
        .await
        .map_err(|err| Error::configuration(format!("Dagger backend failed: {err}")))?;

        Ok(TaskResult {
            name: task_name,
            exit_code: Some(exit_code),
            stdout: if capture_output { stdout } else { String::new() },
            stderr: if capture_output { stderr } else { String::new() },
            success: exit_code == 0,
        })
    }

    fn name(&self) -> &'static str {
        "dagger"
    }
}

#[cfg(not(feature = "dagger-backend"))]
#[async_trait]
impl TaskBackend for DaggerBackend {
    async fn execute(
        &self,
        _name: &str,
        _task: &Task,
        _environment: &Environment,
        _project_root: &Path,
        _capture_output: bool,
    ) -> Result<TaskResult> {
        Err(Error::configuration(
            "Dagger backend not available; rebuild with the 'dagger-backend' feature to enable it"
                .to_string(),
        ))
    }

    fn name(&self) -> &'static str {
        "dagger"
    }
}

pub fn create_backend(
    config: Option<&BackendConfig>,
    project_root: std::path::PathBuf,
    cli_backend: Option<&str>,
) -> Box<dyn TaskBackend> {
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
             let image = config.and_then(|c| c.options.as_ref()).and_then(|o| o.image.clone());
             Box::new(DaggerBackend::new(image, project_root))
        },
        _ => Box::new(HostBackend::new()),
    }
}

pub fn should_use_dagger(
    config: Option<&BackendConfig>,
    cli_backend: Option<&str>,
) -> bool {
    let backend_type = if let Some(b) = cli_backend {
        b
    } else if let Some(c) = config {
        &c.backend_type
    } else {
        "host"
    };
    
    backend_type == "dagger"
}
