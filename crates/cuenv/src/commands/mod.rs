pub mod ci;
pub mod env;
pub(crate) mod env_file;
pub mod exec;
pub mod export;
pub mod handler;
pub mod hooks;
pub mod info;
mod module_utils;
pub mod owners;
pub mod release;
pub mod secrets;
pub mod sync;
pub mod task;
pub mod task_list;
pub mod task_picker;
pub mod version;

pub(crate) use module_utils::convert_engine_error;
pub use module_utils::{ModuleGuard, relative_path_from_root};

use crate::cli::{StatusFormat, SyncCommands};
use crate::events::{Event, EventSender};
use clap_complete::Shell;
use cuengine::ModuleEvalOptions;
use cuenv_core::{InstanceKind, ModuleEvaluation, Result};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio::time::{Duration, sleep};
use tracing::{Level, event};

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Command {
    Version {
        format: String,
    },
    Info {
        /// None = recursive (./...), Some(path) = specific directory only
        path: Option<String>,
        package: String,
        meta: bool,
    },
    EnvPrint {
        path: String,
        package: String,
        format: String,
        environment: Option<String>,
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
    EnvList {
        path: String,
        package: String,
        format: String,
    },
    Task {
        path: String,
        package: String,
        name: Option<String>,
        labels: Vec<String>,
        environment: Option<String>,
        format: String,
        materialize_outputs: Option<String>,
        show_cache_path: bool,
        backend: Option<String>,
        tui: bool,
        interactive: bool,
        help: bool,
        all: bool,
        task_args: Vec<String>,
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
        dynamic: Option<String>,
        from: Option<String>,
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
        json: bool,
    },
    ChangesetFromCommits {
        path: String,
        since: Option<String>,
    },
    ReleaseVersion {
        path: String,
        dry_run: bool,
    },
    ReleasePublish {
        path: String,
        dry_run: bool,
    },
    ReleaseBinaries {
        path: String,
        dry_run: bool,
        backends: Option<Vec<String>>,
        build_only: bool,
        package_only: bool,
        publish_only: bool,
        targets: Option<Vec<String>>,
        version: Option<String>,
    },
    Completions {
        shell: Shell,
    },
    Sync {
        subcommand: Option<SyncCommands>,
        path: String,
        package: String,
        dry_run: bool,
        check: bool,
        all: bool,
    },
    SecretsSetup {
        provider: crate::cli::SecretsProvider,
        wasm_url: Option<String>,
    },
}

/// Executes CLI commands with centralized module evaluation and event handling.
///
/// The `CommandExecutor` provides lazy-loading of CUE module evaluation, ensuring
/// that the module is only loaded when a command actually needs CUE access.
/// This avoids startup overhead for simple commands like `version` or `completions`.
#[allow(dead_code)]
pub struct CommandExecutor {
    event_sender: EventSender,
    /// Lazy-loaded module evaluation, cached after first access
    module: Mutex<Option<ModuleEvaluation>>,
    /// The CUE package name to evaluate (typically "cuenv")
    package: String,
}

#[allow(dead_code)]
impl CommandExecutor {
    /// Create a new executor with the specified event sender and package name.
    pub fn new(event_sender: EventSender, package: String) -> Self {
        Self {
            event_sender,
            module: Mutex::new(None),
            package,
        }
    }

    /// Get the CUE package name used for evaluation.
    #[allow(dead_code)]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Get or load the module evaluation (cached after first call).
    ///
    /// This method lazily loads the CUE module on first access and caches it
    /// for subsequent calls. Commands that don't need CUE evaluation
    /// (version, completions, etc.) never trigger this load.
    ///
    /// # Arguments
    /// * `path` - Directory to start searching for module root
    ///
    /// # Returns
    /// A `ModuleGuard` that provides direct access to the `ModuleEvaluation`
    pub fn get_module(&self, path: &Path) -> Result<ModuleGuard<'_>> {
        let mut guard = self
            .module
            .lock()
            .map_err(|_| cuenv_core::Error::configuration("Failed to acquire module lock"))?;

        if guard.is_none() {
            let module_root = env_file::find_cue_module_root(path).ok_or_else(|| {
                cuenv_core::Error::configuration(format!(
                    "No CUE module found (looking for cue.mod/) starting from: {}",
                    path.display()
                ))
            })?;

            // Evaluate the entire module recursively
            let options = ModuleEvalOptions {
                recursive: true,
                ..Default::default()
            };
            let raw = cuengine::evaluate_module(&module_root, &self.package, Some(options))
                .map_err(convert_engine_error)?;

            *guard = Some(ModuleEvaluation::from_raw(
                module_root,
                raw.instances,
                raw.projects,
            ));
        }

        Ok(ModuleGuard { guard })
    }

    /// Get the module root path if the module has been loaded.
    ///
    /// Returns `None` if `get_module` hasn't been called yet.
    #[allow(dead_code)]
    pub fn module_root(&self) -> Option<PathBuf> {
        self.module
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|m| m.root.clone()))
    }

    /// Compute the relative path from module root to target directory.
    ///
    /// This is a convenience wrapper around `relative_path_from_root` that
    /// uses the cached module root. Returns an error if the module hasn't
    /// been loaded yet.
    #[allow(dead_code)]
    pub fn relative_path(&self, target: &Path) -> Result<PathBuf> {
        let root = self.module_root().ok_or_else(|| {
            cuenv_core::Error::configuration("Module not loaded; call get_module first")
        })?;
        Ok(relative_path_from_root(&root, target))
    }

    /// Check if a path is a Project (vs Base) using schema unification.
    ///
    /// This uses the CUE schema verification performed during module evaluation
    /// to determine if an instance conforms to `schema.#Project`.
    #[allow(dead_code)]
    pub fn is_project(&self, path: &Path) -> bool {
        self.module
            .lock()
            .ok()
            .and_then(|guard| {
                guard
                    .as_ref()
                    .and_then(|m| m.get(path).map(|i| i.kind == InstanceKind::Project))
            })
            .unwrap_or(false)
    }

    /// Execute a command with automatic event lifecycle management.
    #[allow(clippy::too_many_lines)]
    pub async fn execute(&self, command: Command) -> Result<()> {
        use handler::CommandRunner;

        match command {
            // Version command has special progress events - keep inline
            Command::Version { format: _ } => self.execute_version().await,

            // Commands using handler trait pattern
            Command::EnvPrint {
                path,
                package,
                format,
                environment,
            } => {
                self.run_command(handler::EnvPrintHandler {
                    path,
                    package,
                    format,
                    environment,
                })
                .await
            }
            Command::EnvList {
                path,
                package,
                format,
            } => {
                self.run_command(handler::EnvListHandler {
                    path,
                    package,
                    format,
                })
                .await
            }
            Command::EnvLoad { path, package } => {
                self.run_command(handler::EnvLoadHandler { path, package })
                    .await
            }
            Command::EnvStatus {
                path,
                package,
                wait,
                timeout,
                format,
            } => {
                self.run_command(handler::EnvStatusHandler {
                    path,
                    package,
                    wait,
                    timeout,
                    format,
                })
                .await
            }
            Command::EnvCheck {
                path,
                package,
                shell,
            } => {
                self.run_command(handler::EnvCheckHandler {
                    path,
                    package,
                    shell,
                })
                .await
            }
            Command::EnvInspect { path, package } => {
                self.run_command(handler::EnvInspectHandler { path, package })
                    .await
            }
            Command::Allow {
                path,
                package,
                note,
                yes,
            } => {
                self.run_command(handler::AllowHandler {
                    path,
                    package,
                    note,
                    yes,
                })
                .await
            }
            Command::Deny { path, package, all } => {
                self.run_command(handler::DenyHandler { path, package, all })
                    .await
            }
            Command::Export { shell, package } => {
                self.run_command(handler::ExportHandler { shell, package })
                    .await
            }
            Command::Exec {
                path,
                package,
                command,
                args,
                environment,
            } => {
                self.run_command(handler::ExecHandler {
                    path,
                    package,
                    command,
                    args,
                    environment,
                })
                .await
            }
            Command::Task {
                path,
                package,
                name,
                labels,
                environment,
                format,
                materialize_outputs,
                show_cache_path,
                backend,
                tui,
                interactive,
                help,
                all,
                task_args,
            } => {
                self.run_command(handler::TaskHandler {
                    path,
                    package,
                    name,
                    labels,
                    environment,
                    format,
                    materialize_outputs,
                    show_cache_path,
                    backend,
                    tui,
                    interactive,
                    help,
                    all,
                    task_args,
                })
                .await
            }
            Command::Ci {
                dry_run,
                pipeline,
                dynamic,
                from,
            } => {
                self.run_command(handler::CiHandler {
                    dry_run,
                    pipeline,
                    dynamic,
                    from,
                })
                .await
            }
            Command::Sync {
                subcommand,
                path,
                package,
                dry_run,
                check,
                ..
            } => {
                self.run_command(handler::SyncHandler {
                    subcommand,
                    path,
                    package,
                    dry_run,
                    check,
                })
                .await
            }
            Command::ShellInit { shell } => {
                handler::ShellInitHandler { shell }.execute_sync(self);
                Ok(())
            }

            // Commands handled directly in main.rs
            Command::Tui
            | Command::Web { .. }
            | Command::Completions { .. }
            | Command::Info { .. }
            | Command::ChangesetAdd { .. }
            | Command::ChangesetStatus { .. }
            | Command::ChangesetFromCommits { .. }
            | Command::ReleaseVersion { .. }
            | Command::ReleasePublish { .. }
            | Command::ReleaseBinaries { .. }
            | Command::SecretsSetup { .. } => Ok(()),
        }
    }

    /// Execute version command with progress events.
    async fn execute_version(&self) -> Result<()> {
        let command_name = "version";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        for i in 0..=5 {
            #[allow(clippy::cast_precision_loss)]
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

        let version_info = version::get_version_info();

        self.send_event(Event::CommandComplete {
            command: command_name.to_string(),
            success: true,
            output: version_info,
        });

        Ok(())
    }

    pub(crate) fn send_event(&self, event: Event) {
        if let Err(e) = self.event_sender.send(event) {
            event!(Level::ERROR, "Failed to send event: {}", e);
        }
    }
}

#[cfg(test)]
mod tests;
