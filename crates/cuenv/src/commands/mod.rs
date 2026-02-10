/// Interactive changeset picker for selecting changes to include.
pub mod changeset_picker;
/// CI pipeline integration and generation commands.
pub mod ci;
/// Environment management commands (print, load, status, etc.).
pub mod env;
/// CUE module and env.cue file discovery utilities.
pub mod env_file;
/// Execute arbitrary commands within the cuenv environment.
pub mod exec;
/// Export environment variables to shell format.
pub mod export;
/// Format code based on formatters configuration.
pub mod fmt;
/// Git hooks generation and management.
pub mod git_hooks;
/// Command handler trait and implementations for command dispatch.
pub mod handler;
/// Hook execution for environment lifecycle events.
pub mod hooks;
/// Project information and metadata display.
pub mod info;
mod module_utils;
/// Release management commands (prepare, version, publish, binaries).
pub mod release;
/// Secrets provider setup and management.
pub mod secrets;
/// Synchronization commands for codegen, CI, and other providers.
pub mod sync;
/// Task execution and orchestration commands.
pub mod task;
/// Task listing and discovery utilities.
pub mod task_list;
/// Interactive task picker for selecting tasks to run.
pub mod task_picker;
/// Tools command for multi-source tool management.
pub mod tools;
/// Version information and display.
pub mod version;

pub use module_utils::convert_engine_error;
pub use module_utils::{ModuleGuard, relative_path_from_root};

use crate::cli::StatusFormat;
use crate::events::{Event, EventSender};
use clap_complete::Shell;
use cuengine::ModuleEvalOptions;
use cuenv_core::DryRun;
use cuenv_core::cue::discovery::{adjust_meta_key_path, compute_relative_path, format_eval_errors};
use cuenv_core::{InstanceKind, ModuleEvaluation, Result};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio::time::{Duration, sleep};

/// Represents all available CLI commands with their associated parameters.
///
/// Each variant corresponds to a subcommand in the CLI and contains
/// all the configuration needed to execute that command.
#[derive(Debug, Clone)]
pub enum Command {
    /// Display version information in the specified format.
    Version {
        /// Output format (e.g., "text", "json").
        format: String,
    },
    /// Display project information and metadata.
    Info {
        /// None = recursive (./...), Some(path) = specific directory only
        path: Option<String>,
        /// CUE package name to evaluate.
        package: String,
        /// Whether to show extended metadata.
        meta: bool,
    },
    /// Print resolved environment variables.
    EnvPrint {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Output format (e.g., "text", "json", "export").
        format: String,
        /// Optional environment name to select.
        environment: Option<String>,
    },
    /// Load environment variables into the current shell.
    EnvLoad {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
    },
    /// Check the status of environment variables and secrets.
    EnvStatus {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Whether to wait for all secrets to become available.
        wait: bool,
        /// Timeout in seconds when waiting.
        timeout: u64,
        /// Output format for status display.
        format: StatusFormat,
    },
    /// Inspect environment configuration details.
    EnvInspect {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
    },
    /// Check environment configuration for errors.
    EnvCheck {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Target shell type for compatibility checking.
        shell: crate::cli::ShellType,
    },
    /// List available environments in the project.
    EnvList {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Output format (e.g., "text", "json").
        format: String,
    },
    /// Execute one or more tasks with dependency resolution.
    Task {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Specific task name to run (None for interactive picker).
        name: Option<String>,
        /// Label filters to select tasks by labels.
        labels: Vec<String>,
        /// Optional environment name for task execution.
        environment: Option<String>,
        /// Output format (e.g., "text", "json").
        format: String,
        /// Path to materialize task outputs to.
        materialize_outputs: Option<String>,
        /// Whether to show the cache path for tasks.
        show_cache_path: bool,
        /// Execution backend override (e.g., "dagger").
        backend: Option<String>,
        /// Whether to use the TUI for task execution.
        tui: bool,
        /// Whether to run in interactive mode.
        interactive: bool,
        /// Whether to show help for the task.
        help: bool,
        /// Whether to run all available tasks.
        all: bool,
        /// Whether to skip running task dependencies.
        skip_dependencies: bool,
        /// Dry run mode: export DAG without executing.
        dry_run: DryRun,
        /// Additional arguments to pass to the task.
        task_args: Vec<String>,
    },
    /// Execute an arbitrary command within the cuenv environment.
    Exec {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// The command to execute.
        command: String,
        /// Arguments to pass to the command.
        args: Vec<String>,
        /// Optional environment name to use.
        environment: Option<String>,
    },
    /// Format code based on formatters configuration.
    Fmt {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Apply formatting changes (default is check mode).
        fix: bool,
        /// Only run specific formatters.
        only: Option<Vec<String>>,
    },
    /// Initialize shell integration for the specified shell.
    ShellInit {
        /// Target shell type for initialization.
        shell: crate::cli::ShellType,
    },
    /// Allow a project's hooks to run.
    Allow {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Optional note explaining why the project is allowed.
        note: Option<String>,
        /// Skip confirmation prompt.
        yes: bool,
    },
    /// Deny a project's hooks from running.
    Deny {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
    },
    /// Export environment variables in shell format.
    Export {
        /// Target shell format (e.g., "bash", "zsh", "fish").
        shell: Option<String>,
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
    },
    /// Run CI pipeline generation or execution.
    Ci {
        /// CI command arguments.
        args: ci::CiArgs,
    },
    /// Launch the terminal user interface.
    Tui,
    /// Launch the web-based user interface.
    Web {
        /// Port number for the web server.
        port: u16,
        /// Host address to bind to.
        host: String,
    },
    /// Add a new changeset entry.
    ChangesetAdd {
        /// Path to the repository root.
        path: String,
        /// Summary of the changes.
        summary: Option<String>,
        /// Detailed description of the changes.
        description: Option<String>,
        /// List of (package, bump type) pairs.
        packages: Vec<(String, String)>,
    },
    /// Show the status of pending changesets.
    ChangesetStatus {
        /// Path to the repository root.
        path: String,
        /// Output in JSON format.
        json: bool,
    },
    /// Generate changesets from commit history.
    ChangesetFromCommits {
        /// Path to the repository root.
        path: String,
        /// Git ref to start from.
        since: Option<String>,
    },
    /// Prepare a release by processing changesets.
    ReleasePrepare {
        /// Path to the repository root.
        path: String,
        /// Git ref to start from.
        since: Option<String>,
        /// Perform a dry run without making changes.
        dry_run: DryRun,
        /// Branch name for the release PR.
        branch: String,
        /// Skip creating a pull request.
        no_pr: bool,
    },
    /// Update version numbers based on changesets.
    ReleaseVersion {
        /// Path to the repository root.
        path: String,
        /// Perform a dry run without making changes.
        dry_run: DryRun,
    },
    /// Publish packages to their registries.
    ReleasePublish {
        /// Path to the repository root.
        path: String,
        /// Perform a dry run without making changes.
        dry_run: DryRun,
    },
    /// Build and publish release binaries.
    ReleaseBinaries {
        /// Path to the repository root.
        path: String,
        /// Perform a dry run without making changes.
        dry_run: DryRun,
        /// Specific backends to use for building.
        backends: Option<Vec<String>>,
        /// Only build binaries, skip packaging and publishing.
        build_only: bool,
        /// Only package binaries, skip building and publishing.
        package_only: bool,
        /// Only publish binaries, skip building and packaging.
        publish_only: bool,
        /// Specific target platforms to build for.
        targets: Option<Vec<String>>,
        /// Version string override.
        version: Option<String>,
    },
    /// Generate shell completion scripts.
    Completions {
        /// Target shell for completions.
        shell: Shell,
    },
    /// Synchronize generated files with their sources.
    Sync {
        /// Provider name (None = sync all providers)
        subcommand: Option<String>,
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Operation mode (write, dry-run, check).
        mode: sync::SyncMode,
        /// Scope (single path or entire workspace).
        scope: handler::SyncScope,
        /// Show diff for codegen (codegen-specific).
        show_diff: bool,
        /// CI provider filter.
        ci_provider: Option<String>,
        /// Tools to force re-resolution for (lock-specific).
        /// - `None`: use cached resolutions from lockfile
        /// - `Some(vec![])`: re-resolve ALL tools (`-u` with no args)
        /// - `Some(vec!["bun"])`: re-resolve only specified tools
        update_tools: Option<Vec<String>>,
    },
    /// Set up a secrets provider for runtime secret resolution.
    SecretsSetup {
        /// The secrets provider to configure.
        provider: crate::cli::SecretsProvider,
        /// Custom WASM plugin URL for the provider.
        wasm_url: Option<String>,
    },
    /// Activate OCI runtime binaries.
    RuntimeOciActivate,
    /// Download tools for current platform.
    ToolsDownload,
    /// Activate tools (output shell exports).
    ToolsActivate,
    /// List configured tools.
    ToolsList,
}

/// Executes CLI commands with centralized module evaluation and event handling.
///
/// The `CommandExecutor` provides lazy-loading of CUE module evaluation, ensuring
/// that the module is only loaded when a command actually needs CUE access.
/// This avoids startup overhead for simple commands like `version` or `completions`.
pub struct CommandExecutor {
    /// Channel sender for broadcasting events to UI renderers.
    event_sender: EventSender,
    /// Lazy-loaded module evaluation, cached after first access.
    module: Mutex<Option<ModuleEvaluation>>,
    /// The CUE package name to evaluate (typically "cuenv").
    package: String,
}

impl CommandExecutor {
    /// Create a new executor with the specified event sender and package name.
    #[must_use]
    pub const fn new(event_sender: EventSender, package: String) -> Self {
        Self {
            event_sender,
            module: Mutex::new(None),
            package,
        }
    }

    /// Get the CUE package name used for evaluation.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Get or load the module evaluation (cached after first call).
    ///
    /// This method lazily loads the CUE module on first access and caches it
    /// for subsequent calls. Commands that don't need CUE evaluation
    /// (version, completions, etc.) never trigger this load.
    ///
    /// Uses filesystem discovery to find all env.cue files and evaluates each
    /// directory individually with `recursive: false`. This avoids CUE's
    /// `./...:package` pattern which hangs when directories contain mixed packages.
    ///
    /// # Arguments
    /// * `path` - Directory to start searching for module root
    ///
    /// # Returns
    /// A `ModuleGuard` that provides direct access to the `ModuleEvaluation`
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The module lock cannot be acquired (poisoned mutex)
    /// - No CUE module root (cue.mod/) is found starting from the given path
    /// - No instances could be evaluated (all directories failed)
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

            // Discover all directories with env.cue files matching our package
            let env_cue_dirs = env_file::discover_env_cue_directories(&module_root, &self.package);

            if env_cue_dirs.is_empty() {
                return Err(cuenv_core::Error::configuration(format!(
                    "No env.cue files with package '{}' found in module: {}",
                    self.package,
                    module_root.display()
                )));
            }

            // Evaluate each directory individually (non-recursive)
            let mut all_instances = std::collections::HashMap::new();
            let mut all_projects = Vec::new();
            let mut all_meta = std::collections::HashMap::new();
            let mut eval_errors = Vec::new();

            for dir in env_cue_dirs {
                let options = ModuleEvalOptions {
                    recursive: false,
                    with_references: true,
                    target_dir: Some(dir.to_string_lossy().to_string()),
                    ..Default::default()
                };

                // Compute relative path once for this directory
                let dir_rel_path = compute_relative_path(&dir, &module_root);

                match cuengine::evaluate_module(&module_root, &self.package, Some(&options)) {
                    Ok(raw) => {
                        // Merge instances (key by relative path from module_root)
                        for (path_str, value) in raw.instances {
                            // Convert to relative path for consistent keying
                            let rel_path = if path_str == "." {
                                dir_rel_path.clone()
                            } else {
                                path_str
                            };
                            all_instances.insert(rel_path.clone(), value);
                        }

                        for project_path in raw.projects {
                            let rel_project_path = if project_path == "." {
                                dir_rel_path.clone()
                            } else {
                                project_path
                            };
                            if !all_projects.contains(&rel_project_path) {
                                all_projects.push(rel_project_path);
                            }
                        }

                        // Merge meta with adjusted paths
                        for (meta_key, meta_value) in raw.meta {
                            let adjusted_key = adjust_meta_key_path(&meta_key, &dir_rel_path);
                            all_meta.insert(adjusted_key, meta_value);
                        }
                    }
                    Err(e) => {
                        // Log warning but continue - some directories may fail
                        tracing::warn!(
                            dir = %dir.display(),
                            error = %e,
                            "Failed to evaluate env.cue - skipping directory"
                        );
                        eval_errors.push((dir, e));
                    }
                }
            }

            if all_instances.is_empty() {
                let error_summary = format_eval_errors(&eval_errors);
                return Err(cuenv_core::Error::configuration(format!(
                    "No instances could be evaluated. All directories failed:\n{error_summary}"
                )));
            }

            // Convert meta to reference map for dependsOn resolution
            let references = if all_meta.is_empty() {
                None
            } else {
                Some(
                    all_meta
                        .into_iter()
                        .filter_map(|(k, v)| v.reference.map(|r| (k, r)))
                        .collect(),
                )
            };

            *guard = Some(ModuleEvaluation::from_raw(
                module_root,
                all_instances,
                all_projects,
                references,
            ));
        }

        Ok(ModuleGuard { guard })
    }

    /// Get the module root path if the module has been loaded.
    ///
    /// Returns `None` if `get_module` hasn't been called yet.
    #[must_use]
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
    ///
    /// # Errors
    ///
    /// Returns an error if the module has not been loaded yet (call `get_module` first).
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
    #[must_use]
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
    ///
    /// # Errors
    ///
    /// Returns an error if the command execution fails. The specific error
    /// depends on the command being executed:
    /// - Configuration errors for invalid paths or packages
    /// - CUE evaluation errors for malformed CUE files
    /// - Task execution errors for failed commands
    /// - I/O errors for file system operations
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
            Command::Deny { path, package } => {
                self.run_command(handler::DenyHandler { path, package })
                    .await
            }
            Command::Export {
                shell,
                path,
                package,
            } => {
                self.run_command(handler::ExportHandler {
                    shell,
                    path,
                    package,
                })
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
                skip_dependencies,
                dry_run,
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
                    skip_dependencies,
                    dry_run,
                    task_args,
                })
                .await
            }
            Command::Ci { args } => self.run_command(handler::CiHandler { args }).await,
            Command::Sync {
                subcommand,
                path,
                package,
                mode,
                scope,
                show_diff,
                ci_provider,
                update_tools,
            } => {
                self.run_command(handler::SyncHandler {
                    subcommand,
                    path,
                    package,
                    mode,
                    scope,
                    show_diff,
                    ci_provider,
                    update_tools,
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
            | Command::Fmt { .. }
            | Command::ChangesetAdd { .. }
            | Command::ChangesetStatus { .. }
            | Command::ChangesetFromCommits { .. }
            | Command::ReleasePrepare { .. }
            | Command::ReleaseVersion { .. }
            | Command::ReleasePublish { .. }
            | Command::ReleaseBinaries { .. }
            | Command::SecretsSetup { .. }
            | Command::RuntimeOciActivate
            | Command::ToolsDownload
            | Command::ToolsActivate
            | Command::ToolsList => Ok(()),
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

    /// Send an event to all registered UI renderers.
    ///
    /// This method broadcasts events through the event channel. If sending fails
    /// (e.g., all receivers have been dropped), the error is silently ignored
    /// since this is expected when the channel is not being consumed.
    pub(crate) fn send_event(&self, event: Event) {
        // Silently ignore send failures - this is expected when no receiver is attached
        let _ = self.event_sender.send(event);
    }
}

#[cfg(test)]
mod tests;
