pub mod env;
pub(crate) mod env_file;
pub mod exec;
pub mod export;
pub mod hooks;
pub mod release;
pub mod task;
pub mod version;

pub mod ci_cmd {
    use crate::commands::task;
    use async_trait::async_trait;
    use cuenv_ci::executor::{TaskRunner, run_ci};
    use cuenv_core::Result;
    use std::sync::Arc;

    struct CliTaskRunner;

    #[async_trait]
    impl TaskRunner for CliTaskRunner {
        async fn run_task(&self, project_root: &std::path::Path, task_name: &str) -> Result<()> {
            let path_str = project_root.to_string_lossy().to_string();
            // We call execute_task. Note that execute_task expects path to folder with CUE files.
            // It handles loading the config itself.
            // We pass 'cuenv' as package name, matching what we used in discovery.
            // The runner should probably reuse the loaded config if possible, but execute_task loads it again.
            // For now this is fine.
            task::execute_task(
                &path_str,
                "cuenv", // package
                Some(task_name),
                None,  // environment
                false, // capture_output
                None,  // materialize_outputs
                false, // show_cache_path
                None,  // backend
                false, // help
            )
            .await
            .map(|_| ())
        }
    }

    pub async fn execute_ci(
        dry_run: bool,
        pipeline: Option<String>,
        generate: Option<String>,
    ) -> Result<()> {
        if let Some(provider) = generate {
            if provider != "github" {
                return Err(cuenv_core::Error::configuration(format!(
                    "Unsupported CI provider: {provider}. Currently only 'github' is supported."
                )));
            }

            println!("Generating workflow for: {provider}");

            let workflow_content = r#"name: CI

on:
  push:
    branches: ["main"]
  pull_request:
    branches: ["main"]

jobs:
  ci:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - uses: cachix/install-nix-action@v22
        with:
          nix_path: nixpkgs=channel:nixos-unstable

      - name: Install cuenv
        run: curl -fsSL https://cuenv.sh/install | sh

      - name: Run CI
        run: cuenv ci
"#;

            let workflows_dir = std::path::Path::new(".github/workflows");
            if !workflows_dir.exists() {
                std::fs::create_dir_all(workflows_dir).map_err(|e| cuenv_core::Error::Io {
                    source: e,
                    path: Some(workflows_dir.to_path_buf().into_boxed_path()),
                    operation: "create directory".to_string(),
                })?;
            }

            let workflow_path = workflows_dir.join("ci.yml");
            std::fs::write(&workflow_path, workflow_content).map_err(|e| {
                cuenv_core::Error::Io {
                    source: e,
                    path: Some(workflow_path.clone().into_boxed_path()),
                    operation: "write workflow file".to_string(),
                }
            })?;

            println!(
                "Generated GitHub Actions workflow at: {}",
                workflow_path.display()
            );
            return Ok(());
        }

        let runner = Arc::new(CliTaskRunner);
        run_ci(dry_run, pipeline, runner).await
    }
}

use crate::cli::StatusFormat;
use crate::events::{Event, EventSender};
use cuenv_core::Result;
use tokio::time::{Duration, sleep};
use tracing::{Level, event};

#[derive(Debug, Clone)]
pub enum Command {
    Version {
        format: String,
    },
    EnvPrint {
        path: String,
        package: String,
        format: String,
    },
    EnvLoad {
        path: String,
        package: String,
    },
    EnvStatus {
        path: String,
        package: String,
        wait: bool,
        timeout: u64,
        format: StatusFormat,
    },
    EnvInspect {
        path: String,
        package: String,
    },
    EnvCheck {
        path: String,
        package: String,
        shell: crate::cli::ShellType,
    },
    Task {
        path: String,
        package: String,
        name: Option<String>,
        environment: Option<String>,
        materialize_outputs: Option<String>,
        show_cache_path: bool,
        backend: Option<String>,
        help: bool,
    },
    Exec {
        path: String,
        package: String,
        command: String,
        args: Vec<String>,
        environment: Option<String>,
    },
    ShellInit {
        shell: crate::cli::ShellType,
    },
    Allow {
        path: String,
        package: String,
        note: Option<String>,
        yes: bool,
    },
    Deny {
        path: String,
        package: String,
        all: bool,
    },
    Export {
        shell: Option<String>,
        package: String,
    },
    Ci {
        dry_run: bool,
        pipeline: Option<String>,
        generate: Option<String>,
    },
    Tui,
    Web {
        port: u16,
        host: String,
    },
    ChangesetAdd {
        path: String,
        summary: String,
        description: Option<String>,
        packages: Vec<(String, String)>,
    },
    ChangesetStatus {
        path: String,
    },
    ReleaseVersion {
        path: String,
        dry_run: bool,
    },
    ReleasePublish {
        path: String,
        dry_run: bool,
    },
}

#[allow(dead_code)]
pub struct CommandExecutor {
    event_sender: EventSender,
}

#[allow(dead_code)]
impl CommandExecutor {
    pub fn new(event_sender: EventSender) -> Self {
        Self { event_sender }
    }

    pub async fn execute(&self, command: Command) -> Result<()> {
        match command {
            Command::Version { format } => self.execute_version(format).await,
            Command::EnvPrint {
                path,
                package,
                format,
            } => self.execute_env_print(path, package, format).await,
            Command::Task {
                path,
                package,
                name,
                environment,
                materialize_outputs,
                show_cache_path,
                backend,
                help,
            } => {
                self.execute_task(
                    path,
                    package,
                    name,
                    environment,
                    materialize_outputs,
                    show_cache_path,
                    backend,
                    help,
                )
                .await
            }
            Command::Exec {
                path,
                package,
                command,
                args,
                environment,
            } => {
                self.execute_exec(path, package, command, args, environment)
                    .await
            }
            Command::EnvLoad { path, package } => self.execute_env_load(path, package).await,
            Command::EnvStatus {
                path,
                package,
                wait,
                timeout,
                format,
            } => {
                self.execute_env_status(path, package, wait, timeout, format)
                    .await
            }
            Command::EnvInspect { path, package } => self.execute_env_inspect(path, package).await,
            Command::EnvCheck {
                path,
                package,
                shell,
            } => self.execute_env_check(path, package, shell).await,
            Command::ShellInit { shell } => {
                self.execute_shell_init(shell);
                Ok(())
            }
            Command::Allow {
                path,
                package,
                note,
                yes,
            } => self.execute_allow(path, package, note, yes).await,
            Command::Deny { path, package, all } => self.execute_deny(path, package, all).await,
            Command::Export { shell, package } => self.execute_export(shell, package).await,
            Command::Ci {
                dry_run,
                pipeline,
                generate,
            } => self.execute_ci(dry_run, pipeline, generate).await,
            // Tui, Web, and release commands are handled directly in main.rs
            Command::Tui
            | Command::Web { .. }
            | Command::ChangesetAdd { .. }
            | Command::ChangesetStatus { .. }
            | Command::ReleaseVersion { .. }
            | Command::ReleasePublish { .. } => Ok(()),
        }
    }

    async fn execute_version(&self, _format: String) -> Result<()> {
        let command_name = "version";

        // Send command start event
        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        // Simulate some work with progress updates
        for i in 0..=5 {
            #[allow(clippy::cast_precision_loss)] // Progress calculation for demo purposes
            let progress = i as f32 / 5.0;
            let message = match i {
                0 => "Initializing...".to_string(),
                1 => "Loading version info...".to_string(),
                2 => "Checking build metadata...".to_string(),
                3 => "Gathering system info...".to_string(),
                4 => "Formatting output...".to_string(),
                5 => "Complete".to_string(),
                _ => "Processing...".to_string(),
            };

            self.send_event(Event::CommandProgress {
                command: command_name.to_string(),
                progress,
                message,
            });

            if i < 5 {
                sleep(Duration::from_millis(200)).await;
            }
        }

        // Get version information
        let version_info = version::get_version_info();

        // Send completion event
        self.send_event(Event::CommandComplete {
            command: command_name.to_string(),
            success: true,
            output: version_info,
        });

        Ok(())
    }

    async fn execute_env_print(&self, path: String, package: String, format: String) -> Result<()> {
        let command_name = "env print";

        // Send command start event
        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        // Execute the env print command
        match env::execute_env_print(&path, &package, &format).await {
            Ok(output) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_task(
        &self,
        path: String,
        package: String,
        name: Option<String>,
        environment: Option<String>,
        materialize_outputs: Option<String>,
        show_cache_path: bool,
        backend: Option<String>,
        help: bool,
    ) -> Result<()> {
        let command_name = "task";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        // Execute the task command
        match task::execute_task(
            &path,
            &package,
            name.as_deref(),
            environment.as_deref(),
            false,
            materialize_outputs.as_deref(),
            show_cache_path,
            backend.as_deref(),
            help,
        )
        .await
        {
            Ok(output) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    async fn execute_exec(
        &self,
        path: String,
        package: String,
        command: String,
        args: Vec<String>,
        environment: Option<String>,
    ) -> Result<()> {
        let command_name = "exec";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        // Execute the exec command
        match exec::execute_exec(&path, &package, &command, &args, environment.as_deref()).await {
            Ok(exit_code) => {
                let success = exit_code == 0;
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success,
                    output: format!("Command exited with code {exit_code}"),
                });
                if success {
                    Ok(())
                } else {
                    Err(cuenv_core::Error::configuration(format!(
                        "Command failed with exit code {exit_code}"
                    )))
                }
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    async fn execute_env_load(&self, path: String, package: String) -> Result<()> {
        let command_name = "env load";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        match hooks::execute_env_load(&path, &package).await {
            Ok(output) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    async fn execute_env_status(
        &self,
        path: String,
        package: String,
        wait: bool,
        timeout: u64,
        format: StatusFormat,
    ) -> Result<()> {
        let command_name = "env status";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        match hooks::execute_env_status(&path, &package, wait, timeout, format).await {
            Ok(output) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    async fn execute_env_check(
        &self,
        path: String,
        package: String,
        shell: crate::cli::ShellType,
    ) -> Result<()> {
        let command_name = "env check";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        match hooks::execute_env_check(&path, &package, shell).await {
            Ok(output) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    async fn execute_env_inspect(&self, path: String, package: String) -> Result<()> {
        let command_name = "env inspect";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        match hooks::execute_env_inspect(&path, &package).await {
            Ok(output) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    fn execute_shell_init(&self, shell: crate::cli::ShellType) {
        let command_name = "shell init";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        let output = hooks::execute_shell_init(shell);
        self.send_event(Event::CommandComplete {
            command: command_name.to_string(),
            success: true,
            output,
        });
    }

    async fn execute_allow(
        &self,
        path: String,
        package: String,
        note: Option<String>,
        yes: bool,
    ) -> Result<()> {
        let command_name = "allow";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        match hooks::execute_allow(&path, &package, note, yes).await {
            Ok(output) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    async fn execute_deny(&self, path: String, package: String, all: bool) -> Result<()> {
        let command_name = "deny";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        match hooks::execute_deny(&path, &package, all).await {
            Ok(output) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    /// Execute export command safely
    async fn execute_export(&self, shell: Option<String>, package: String) -> Result<()> {
        let command_name = "export";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        match export::execute_export(shell.as_deref(), &package).await {
            Ok(output) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    async fn execute_ci(
        &self,
        dry_run: bool,
        pipeline: Option<String>,
        generate: Option<String>,
    ) -> Result<()> {
        let command_name = "ci";
        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        match ci_cmd::execute_ci(dry_run, pipeline, generate).await {
            Ok(()) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output: "CI execution completed".to_string(),
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    fn send_event(&self, event: Event) {
        if let Err(e) = self.event_sender.send(event) {
            event!(Level::ERROR, "Failed to send event: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event, EventReceiver};
    use std::time::Duration;
    use tokio::sync::mpsc;
    use tokio::time::timeout;

    fn create_test_executor() -> (CommandExecutor, EventReceiver) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let executor = CommandExecutor::new(sender);
        (executor, receiver)
    }

    #[allow(dead_code)]
    async fn collect_events(mut receiver: EventReceiver, count: usize) -> Vec<Event> {
        let mut events = Vec::new();
        for _ in 0..count {
            if let Ok(Some(event)) = timeout(Duration::from_millis(500), receiver.recv()).await {
                events.push(event);
            }
        }
        events
    }

    #[tokio::test]
    async fn test_command_executor_new() {
        let (sender, _receiver) = mpsc::unbounded_channel();
        let executor = CommandExecutor::new(sender);
        assert!(matches!(executor, CommandExecutor { .. }));
    }

    #[tokio::test]
    async fn test_execute_version_command() {
        let (executor, mut receiver) = create_test_executor();

        let handle = tokio::spawn(async move {
            executor
                .execute(Command::Version {
                    format: "simple".to_string(),
                })
                .await
        });

        // Collect events
        let mut events = Vec::new();
        while let Ok(Some(event)) = timeout(Duration::from_millis(1500), receiver.recv()).await {
            let is_complete = matches!(&event, Event::CommandComplete { .. });
            events.push(event);

            // Break when we receive CommandComplete
            if is_complete {
                break;
            }
        }

        let result = handle.await.unwrap();
        assert!(result.is_ok());

        // Verify we got start, progress, and complete events
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::CommandStart { command } if command == "version"))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::CommandProgress { .. }))
        );
        assert!(events.iter().any(|e| matches!(e, Event::CommandComplete { command, success: true, .. } if command == "version")));
    }

    #[tokio::test]
    async fn test_execute_version_progress_events() {
        let (executor, mut receiver) = create_test_executor();

        let handle = tokio::spawn(async move {
            executor
                .execute(Command::Version {
                    format: "simple".to_string(),
                })
                .await
        });

        // Collect progress events
        let mut progress_events = Vec::new();
        while let Ok(Some(event)) = timeout(Duration::from_millis(1500), receiver.recv()).await {
            if let Event::CommandProgress {
                progress, message, ..
            } = event
            {
                progress_events.push((progress, message));
            } else if matches!(event, Event::CommandComplete { .. }) {
                break;
            }
        }

        let result = handle.await.unwrap();
        assert!(result.is_ok());

        // Verify progress sequence
        assert!(!progress_events.is_empty());
        assert!(
            progress_events
                .iter()
                .any(|(_, msg)| msg.contains("Initializing"))
        );
        assert!(
            progress_events
                .iter()
                .any(|(_, msg)| msg.contains("Loading version info"))
        );
        assert!(
            progress_events
                .iter()
                .any(|(_, msg)| msg.contains("Complete"))
        );

        // Verify progress values
        let progress_values: Vec<f32> = progress_events.iter().map(|(p, _)| *p).collect();
        assert!(progress_values.first().unwrap() <= progress_values.last().unwrap());
    }

    #[tokio::test]
    async fn test_execute_env_print_success() {
        let (executor, mut receiver) = create_test_executor();

        // Mock successful env print
        let path = "/tmp/test".to_string();
        let package = "test-package".to_string();
        let format = "json".to_string();

        let handle = tokio::spawn(async move {
            executor
                .execute(Command::EnvPrint {
                    path,
                    package,
                    format,
                })
                .await
        });

        // Collect events
        let mut events = Vec::new();
        while let Ok(Some(event)) = timeout(Duration::from_millis(1500), receiver.recv()).await {
            let is_complete = matches!(&event, Event::CommandComplete { .. });
            events.push(event);
            if is_complete {
                break;
            }
        }

        // Note: This might fail due to actual file system operations
        // In a real test, we'd mock the env::execute_env_print function
        let _ = handle.await.unwrap();

        // Verify start event was sent
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::CommandStart { command } if command == "env print"))
        );
        // Verify complete event was sent (success depends on actual execution)
        assert!(events.iter().any(
            |e| matches!(e, Event::CommandComplete { command, .. } if command == "env print")
        ));
    }

    #[tokio::test]
    async fn test_command_enum_variants() {
        // Test Command enum creation
        let version_cmd = Command::Version {
            format: "simple".to_string(),
        };
        assert!(matches!(version_cmd, Command::Version { .. }));

        let env_cmd = Command::EnvPrint {
            path: "/test/path".to_string(),
            package: "test-pkg".to_string(),
            format: "yaml".to_string(),
        };

        if let Command::EnvPrint {
            path,
            package,
            format,
        } = env_cmd
        {
            assert_eq!(path, "/test/path");
            assert_eq!(package, "test-pkg");
            assert_eq!(format, "yaml");
        } else {
            panic!("Expected EnvPrint variant");
        }
    }

    #[tokio::test]
    async fn test_send_event() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let executor = CommandExecutor::new(sender);

        // Send a test event
        executor.send_event(Event::CommandStart {
            command: "test".to_string(),
        });

        // Verify event was received
        let event = receiver.recv().await.unwrap();
        assert!(matches!(event, Event::CommandStart { command } if command == "test"));
    }

    #[tokio::test]
    async fn test_send_event_with_closed_channel() {
        let (sender, receiver) = mpsc::unbounded_channel();
        let executor = CommandExecutor::new(sender);

        // Close the receiver
        drop(receiver);

        // Send event should not panic, just log error
        executor.send_event(Event::CommandStart {
            command: "test".to_string(),
        });

        // Test passes if no panic occurs
    }

    #[tokio::test]
    async fn test_execute_version_command_flow() {
        let (executor, mut receiver) = create_test_executor();

        let handle =
            tokio::spawn(async move { executor.execute_version("simple".to_string()).await });

        // Verify the complete flow
        let mut has_start = false;
        let mut has_progress = false;
        let mut has_complete = false;

        while let Ok(Some(event)) = timeout(Duration::from_millis(1500), receiver.recv()).await {
            match event {
                Event::CommandStart { command } if command == "version" => has_start = true,
                Event::CommandProgress { command, .. } if command == "version" => {
                    has_progress = true;
                }
                Event::CommandComplete {
                    command,
                    success: true,
                    ..
                } if command == "version" => {
                    has_complete = true;
                    break;
                }
                _ => {}
            }
        }

        let result = handle.await.unwrap();
        assert!(result.is_ok());
        assert!(has_start);
        assert!(has_progress);
        assert!(has_complete);
    }

    #[tokio::test]
    async fn test_command_debug_trait() {
        let cmd = Command::Version {
            format: "simple".to_string(),
        };
        let debug_str = format!("{cmd:?}");
        assert!(debug_str.contains("Version"));

        let cmd = Command::EnvPrint {
            path: "/path".to_string(),
            package: "pkg".to_string(),
            format: "json".to_string(),
        };
        let debug_str = format!("{cmd:?}");
        assert!(debug_str.contains("EnvPrint"));
        assert!(debug_str.contains("/path"));
        assert!(debug_str.contains("pkg"));
        assert!(debug_str.contains("json"));
    }

    #[tokio::test]
    async fn test_command_clone_trait() {
        let original = Command::Version {
            format: "simple".to_string(),
        };
        let cloned = original.clone();
        assert!(matches!(cloned, Command::Version { .. }));

        let original = Command::EnvPrint {
            path: "/test".to_string(),
            package: "test".to_string(),
            format: "toml".to_string(),
        };
        let cloned = original.clone();

        if let Command::EnvPrint {
            path,
            package,
            format,
        } = cloned
        {
            assert_eq!(path, "/test");
            assert_eq!(package, "test");
            assert_eq!(format, "toml");
        } else {
            panic!("Clone failed");
        }
    }
}
