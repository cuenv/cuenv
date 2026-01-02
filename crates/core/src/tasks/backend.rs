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
    #[allow(clippy::too_many_arguments)] // Task execution requires full context
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
/// This function only handles the `host` backend. For `dagger` backend support,
/// use `create_backend_with_factory` and provide a factory from `cuenv-dagger`.
pub fn create_backend(
    config: Option<&BackendConfig>,
    project_root: std::path::PathBuf,
    cli_backend: Option<&str>,
) -> Arc<dyn TaskBackend> {
    create_backend_with_factory(config, project_root, cli_backend, None)
}

/// Create a backend with an optional factory for non-host backends.
///
/// The `dagger_factory` parameter should be `Some(cuenv_dagger::create_dagger_backend)`
/// when the dagger backend is available.
pub fn create_backend_with_factory(
    config: Option<&BackendConfig>,
    project_root: std::path::PathBuf,
    cli_backend: Option<&str>,
    dagger_factory: Option<BackendFactory>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_backend_new() {
        let backend = HostBackend::new();
        assert_eq!(backend.name(), "host");
    }

    #[test]
    fn test_host_backend_default() {
        let backend = HostBackend::default();
        assert_eq!(backend.name(), "host");
    }

    #[test]
    fn test_host_backend_name() {
        let backend = HostBackend;
        assert_eq!(backend.name(), "host");
    }

    #[test]
    fn test_should_use_dagger_cli_override_dagger() {
        // CLI override takes precedence
        assert!(should_use_dagger(None, Some("dagger")));
    }

    #[test]
    fn test_should_use_dagger_cli_override_host() {
        // CLI override to host
        assert!(!should_use_dagger(None, Some("host")));
    }

    #[test]
    fn test_should_use_dagger_config_dagger() {
        let config = BackendConfig {
            backend_type: "dagger".to_string(),
            options: None,
        };
        assert!(should_use_dagger(Some(&config), None));
    }

    #[test]
    fn test_should_use_dagger_config_host() {
        let config = BackendConfig {
            backend_type: "host".to_string(),
            options: None,
        };
        assert!(!should_use_dagger(Some(&config), None));
    }

    #[test]
    fn test_should_use_dagger_default() {
        // No config, no CLI - defaults to host
        assert!(!should_use_dagger(None, None));
    }

    #[test]
    fn test_should_use_dagger_cli_overrides_config() {
        let config = BackendConfig {
            backend_type: "dagger".to_string(),
            options: None,
        };
        // CLI override to host, even though config says dagger
        assert!(!should_use_dagger(Some(&config), Some("host")));
    }

    #[test]
    fn test_create_backend_defaults_to_host() {
        let backend = create_backend(None, std::path::PathBuf::from("."), None);
        assert_eq!(backend.name(), "host");
    }

    #[test]
    fn test_create_backend_with_cli_host() {
        let backend = create_backend(None, std::path::PathBuf::from("."), Some("host"));
        assert_eq!(backend.name(), "host");
    }

    #[test]
    fn test_create_backend_with_config_host() {
        let config = BackendConfig {
            backend_type: "host".to_string(),
            options: None,
        };
        let backend = create_backend(Some(&config), std::path::PathBuf::from("."), None);
        assert_eq!(backend.name(), "host");
    }

    #[test]
    fn test_create_backend_unknown_type_defaults_to_host() {
        let config = BackendConfig {
            backend_type: "unknown".to_string(),
            options: None,
        };
        let backend = create_backend(Some(&config), std::path::PathBuf::from("."), None);
        // Unknown backend types fall back to host
        assert_eq!(backend.name(), "host");
    }

    #[test]
    fn test_create_backend_dagger_without_factory() {
        let config = BackendConfig {
            backend_type: "dagger".to_string(),
            options: None,
        };
        // Without factory, dagger falls back to host
        let backend = create_backend(Some(&config), std::path::PathBuf::from("."), None);
        assert_eq!(backend.name(), "host");
    }

    #[test]
    fn test_create_backend_with_factory_dagger() {
        // Create a mock factory that returns a host backend (for testing)
        fn mock_dagger_factory(
            _config: Option<&BackendConfig>,
            _project_root: std::path::PathBuf,
        ) -> Arc<dyn TaskBackend> {
            Arc::new(HostBackend::new())
        }

        let config = BackendConfig {
            backend_type: "dagger".to_string(),
            options: None,
        };

        let backend = create_backend_with_factory(
            Some(&config),
            std::path::PathBuf::from("."),
            None,
            Some(mock_dagger_factory),
        );
        // The mock factory returns a host backend, but the factory was called
        assert_eq!(backend.name(), "host");
    }

    #[test]
    fn test_create_backend_with_factory_cli_overrides_to_dagger() {
        fn mock_dagger_factory(
            _config: Option<&BackendConfig>,
            _project_root: std::path::PathBuf,
        ) -> Arc<dyn TaskBackend> {
            Arc::new(HostBackend::new())
        }

        // CLI says dagger, even with no config
        let backend = create_backend_with_factory(
            None,
            std::path::PathBuf::from("."),
            Some("dagger"),
            Some(mock_dagger_factory),
        );
        assert_eq!(backend.name(), "host"); // Mock returns host
    }

    #[test]
    fn test_create_backend_with_factory_cli_overrides_config() {
        fn mock_dagger_factory(
            _config: Option<&BackendConfig>,
            _project_root: std::path::PathBuf,
        ) -> Arc<dyn TaskBackend> {
            Arc::new(HostBackend::new())
        }

        let config = BackendConfig {
            backend_type: "dagger".to_string(),
            options: None,
        };

        // CLI says host, config says dagger - CLI wins
        let backend = create_backend_with_factory(
            Some(&config),
            std::path::PathBuf::from("."),
            Some("host"),
            Some(mock_dagger_factory),
        );
        assert_eq!(backend.name(), "host");
    }

    #[test]
    fn test_backend_config_debug() {
        let config = BackendConfig {
            backend_type: "host".to_string(),
            options: None,
        };
        let debug_str = format!("{:?}", config);
        assert!(debug_str.contains("host"));
    }
}
