//! Task backend abstraction for different execution environments
//!
//! This module provides a pluggable backend system for task execution.
//! The default backend is `Host`, which runs tasks directly on the host machine.
//! The `Dagger` backend runs tasks inside containers using Dagger.

use super::{Task, TaskResult};
use crate::config::{BackendConfig, BackendOptions, BackendType};
use crate::environment::Environment;
use crate::{Error, Result};
use async_trait::async_trait;
use std::path::Path;
use std::process::Stdio;
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

    /// Returns the name of this backend (e.g., "host", "dagger")
    fn name(&self) -> &'static str;
}

/// Host backend - executes tasks directly on the host machine
pub struct HostBackend;

impl HostBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for HostBackend {
    fn default() -> Self {
        Self::new()
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
            if shell.command.is_some() && shell.flag.is_some() {
                let shell_command = shell.command.as_ref().unwrap();
                let shell_flag = shell.flag.as_ref().unwrap();
                let resolved_shell = environment.resolve_command(shell_command);
                let mut cmd = Command::new(&resolved_shell);
                cmd.arg(shell_flag);
                if task.args.is_empty() {
                    cmd.arg(&resolved_command);
                } else {
                    let full_command = if task.command.is_empty() {
                        task.args.join(" ")
                    } else {
                        format!("{} {}", resolved_command, task.args.join(" "))
                    };
                    cmd.arg(full_command);
                }
                cmd
            } else {
                let mut cmd = Command::new(&resolved_command);
                for arg in &task.args {
                    cmd.arg(arg);
                }
                cmd
            }
        } else {
            let mut cmd = Command::new(&resolved_command);
            for arg in &task.args {
                cmd.arg(arg);
            }
            cmd
        };

        // Set working directory and environment
        cmd.current_dir(project_root);
        let env_vars = environment.merge_with_system();
        for (k, v) in &env_vars {
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
    /// Default image to use when not specified per-task
    default_image: Option<String>,
    /// Platform to use for containers
    platform: Option<String>,
}

impl DaggerBackend {
    pub fn new(options: Option<&BackendOptions>) -> Self {
        let (default_image, platform) = if let Some(opts) = options {
            (opts.image.clone(), opts.platform.clone())
        } else {
            (None, None)
        };

        Self {
            default_image,
            platform,
        }
    }

    /// Get the image to use for a specific task
    fn get_image_for_task(&self, task: &Task) -> Option<String> {
        // Per-task image takes precedence over default
        task.dagger
            .as_ref()
            .and_then(|d| d.image.clone())
            .or_else(|| self.default_image.clone())
    }
}

#[async_trait]
impl TaskBackend for DaggerBackend {
    async fn execute(
        &self,
        name: &str,
        task: &Task,
        environment: &Environment,
        project_root: &Path,
        capture_output: bool,
    ) -> Result<TaskResult> {
        let image = self.get_image_for_task(task).ok_or_else(|| {
            Error::configuration(format!(
                "Task '{}' requires a Dagger image but none was specified. \
                 Set 'dagger.image' on the task or 'config.backend.options.image' globally.",
                name
            ))
        })?;

        tracing::info!(
            task = %name,
            backend = "dagger",
            image = %image,
            "Executing task in Dagger container"
        );

        // Build the command to run inside the container
        let task_command = if task.args.is_empty() {
            task.command.clone()
        } else {
            format!("{} {}", task.command, task.args.join(" "))
        };

        // For simplicity in this spike, we use docker as the container runtime.
        // Full Dagger SDK integration is out of scope for the spike but would
        // replace this docker-based implementation in the future.
        // This demonstrates the backend abstraction pattern while keeping the spike simple.

        // Use docker as the container runtime for this spike
        let mut docker_cmd = Command::new("docker");
        docker_cmd.arg("run");
        docker_cmd.arg("--rm");

        // Mount the project directory
        docker_cmd
            .arg("-v")
            .arg(format!("{}:/work", project_root.display()));
        docker_cmd.arg("-w").arg("/work");

        // Set environment variables
        let env_vars = environment.merge_with_system();
        for (k, v) in &env_vars {
            docker_cmd.arg("-e").arg(format!("{}={}", k, v));
        }

        // Add platform if specified
        if let Some(ref platform) = self.platform {
            docker_cmd.arg("--platform").arg(platform);
        }

        // Set the image
        docker_cmd.arg(&image);

        // Set the command
        docker_cmd.arg("sh").arg("-c").arg(&task_command);

        // Set working directory for docker command
        docker_cmd.current_dir(project_root);

        // Execute
        if capture_output {
            let output = docker_cmd
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .map_err(|e| Error::Io {
                    source: e,
                    path: None,
                    operation: format!("spawn dagger task {}", name),
                })?;

            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);
            let success = output.status.success();

            if !success {
                tracing::warn!(task = %name, exit = exit_code, "Dagger task failed");
            }

            Ok(TaskResult {
                name: name.to_string(),
                exit_code: Some(exit_code),
                stdout,
                stderr,
                success,
            })
        } else {
            // Stream output directly to terminal
            let status = docker_cmd
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .await
                .map_err(|e| Error::Io {
                    source: e,
                    path: None,
                    operation: format!("spawn dagger task {}", name),
                })?;

            let exit_code = status.code().unwrap_or(-1);
            let success = status.success();

            if !success {
                tracing::warn!(task = %name, exit = exit_code, "Dagger task failed");
            }

            Ok(TaskResult {
                name: name.to_string(),
                exit_code: Some(exit_code),
                stdout: String::new(),
                stderr: String::new(),
                success,
            })
        }
    }

    fn name(&self) -> &'static str {
        "dagger"
    }
}

/// Create a backend instance based on configuration
pub fn create_backend(config: Option<&BackendConfig>) -> Box<dyn TaskBackend> {
    match config {
        Some(cfg) => match cfg.backend_type {
            BackendType::Host => Box::new(HostBackend::new()),
            BackendType::Dagger => Box::new(DaggerBackend::new(cfg.options.as_ref())),
        },
        None => Box::new(HostBackend::new()),
    }
}

/// Check if a task should use the Dagger backend
///
/// A task is a Dagger task if:
/// 1. The run's selected backend is `dagger`, AND
/// 2. Either:
///    - The task has a `dagger` block (with or without image), or
///    - There is a global default image in `config.backend.options.image`
pub fn should_use_dagger(task: &Task, global_config: Option<&BackendConfig>) -> bool {
    if let Some(config) = global_config
        && config.backend_type == BackendType::Dagger
    {
        // Task has explicit dagger block (even if empty)
        let has_task_dagger = task.dagger.is_some();
        // Global has default image
        let has_global_image = config
            .options
            .as_ref()
            .and_then(|o| o.image.as_ref())
            .is_some();
        return has_task_dagger || has_global_image;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_backend_name() {
        let backend = HostBackend::new();
        assert_eq!(backend.name(), "host");
    }

    #[test]
    fn test_dagger_backend_name() {
        let backend = DaggerBackend::new(None);
        assert_eq!(backend.name(), "dagger");
    }

    #[test]
    fn test_dagger_backend_with_options() {
        let options = BackendOptions {
            image: Some("ubuntu:22.04".to_string()),
            platform: Some("linux/amd64".to_string()),
        };
        let backend = DaggerBackend::new(Some(&options));
        assert_eq!(backend.default_image, Some("ubuntu:22.04".to_string()));
        assert_eq!(backend.platform, Some("linux/amd64".to_string()));
    }

    #[test]
    fn test_create_backend_default() {
        let backend = create_backend(None);
        assert_eq!(backend.name(), "host");
    }

    #[test]
    fn test_create_backend_host() {
        let config = BackendConfig {
            backend_type: BackendType::Host,
            options: None,
        };
        let backend = create_backend(Some(&config));
        assert_eq!(backend.name(), "host");
    }

    #[test]
    fn test_create_backend_dagger() {
        let config = BackendConfig {
            backend_type: BackendType::Dagger,
            options: Some(BackendOptions {
                image: Some("alpine:latest".to_string()),
                platform: None,
            }),
        };
        let backend = create_backend(Some(&config));
        assert_eq!(backend.name(), "dagger");
    }

    #[test]
    fn test_should_use_dagger_no_config() {
        let task = Task::default();
        assert!(!should_use_dagger(&task, None));
    }

    #[test]
    fn test_should_use_dagger_host_backend() {
        let task = Task::default();
        let config = BackendConfig {
            backend_type: BackendType::Host,
            options: None,
        };
        assert!(!should_use_dagger(&task, Some(&config)));
    }

    #[test]
    fn test_should_use_dagger_with_task_image() {
        use super::super::DaggerTaskConfig;

        let task = Task {
            dagger: Some(DaggerTaskConfig {
                image: Some("ubuntu:22.04".to_string()),
            }),
            ..Default::default()
        };
        let config = BackendConfig {
            backend_type: BackendType::Dagger,
            options: None,
        };
        assert!(should_use_dagger(&task, Some(&config)));
    }

    #[test]
    fn test_should_use_dagger_with_global_image() {
        let task = Task::default();
        let config = BackendConfig {
            backend_type: BackendType::Dagger,
            options: Some(BackendOptions {
                image: Some("ubuntu:22.04".to_string()),
                platform: None,
            }),
        };
        assert!(should_use_dagger(&task, Some(&config)));
    }

    #[test]
    fn test_should_use_dagger_no_image() {
        let task = Task::default();
        let config = BackendConfig {
            backend_type: BackendType::Dagger,
            options: None,
        };
        // No image configured anywhere, so shouldn't use dagger
        assert!(!should_use_dagger(&task, Some(&config)));
    }

    #[test]
    fn test_should_use_dagger_with_empty_dagger_block_and_global_image() {
        use super::super::DaggerTaskConfig;

        // Task has empty dagger block, but global config has default image
        let task = Task {
            dagger: Some(DaggerTaskConfig { image: None }),
            ..Default::default()
        };
        let config = BackendConfig {
            backend_type: BackendType::Dagger,
            options: Some(BackendOptions {
                image: Some("ubuntu:22.04".to_string()),
                platform: None,
            }),
        };
        // Should use dagger because task has dagger block (even if empty)
        assert!(should_use_dagger(&task, Some(&config)));
    }

    #[test]
    fn test_get_image_for_task_task_takes_precedence() {
        use super::super::DaggerTaskConfig;

        let options = BackendOptions {
            image: Some("global:image".to_string()),
            platform: None,
        };
        let backend = DaggerBackend::new(Some(&options));

        let task = Task {
            dagger: Some(DaggerTaskConfig {
                image: Some("task:image".to_string()),
            }),
            ..Default::default()
        };

        assert_eq!(
            backend.get_image_for_task(&task),
            Some("task:image".to_string())
        );
    }

    #[test]
    fn test_get_image_for_task_falls_back_to_global() {
        let options = BackendOptions {
            image: Some("global:image".to_string()),
            platform: None,
        };
        let backend = DaggerBackend::new(Some(&options));
        let task = Task::default();

        assert_eq!(
            backend.get_image_for_task(&task),
            Some("global:image".to_string())
        );
    }
}
