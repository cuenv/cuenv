/// Build container images defined in CUE configuration.
pub mod build;
/// Interactive changeset picker for selecting changes to include.
pub mod changeset_picker;
/// CI pipeline integration and generation commands.
pub mod ci;
/// Tear down running services.
pub mod down;
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
/// View service logs.
pub mod logs;
mod module_utils;
/// List running services and their status.
pub mod ps;
/// Release management commands (prepare, version, publish, binaries).
pub mod release;
/// Restart one or more services.
pub mod restart;
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
/// Bring up long-running services.
pub mod up;
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
use rayon::prelude::*;
use std::collections::HashMap;
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
    /// Build container images defined in CUE configuration.
    Build {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Image names to build (empty = list all).
        names: Vec<String>,
        /// Label filters to select images by labels.
        labels: Vec<String>,
    },
    /// Bring up long-running services defined in CUE configuration.
    Up {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Specific service names to bring up (empty = all).
        services: Vec<String>,
        /// Label filters to select services by labels.
        labels: Vec<String>,
    },
    /// Tear down running services.
    Down {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Specific service names to bring down (empty = all).
        services: Vec<String>,
    },
    /// View service logs.
    Logs {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Specific service names to view logs for (empty = all).
        services: Vec<String>,
        /// Follow log output.
        follow: bool,
        /// Number of lines to show.
        lines: usize,
    },
    /// List running services and their status.
    Ps {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Output format (table or json).
        output_format: String,
    },
    /// Restart one or more services.
    Restart {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Service names to restart.
        services: Vec<String>,
    },
}

/// Executes CLI commands with centralized module evaluation and event handling.
///
/// The `CommandExecutor` provides lazy-loading of CUE module evaluation, ensuring
/// that the module is only loaded when a command actually needs CUE access.
/// This avoids startup overhead for simple commands like `version` or `completions`.
pub struct CommandExecutor {
    /// Channel sender for broadcasting events to UI renderers.
    event_sender: EventSender,
    /// Path-local module evaluations keyed by canonical target directory.
    local_modules: Mutex<HashMap<PathBuf, ModuleEvaluation>>,
    /// Workspace-wide module evaluations keyed by canonical module root.
    workspace_modules: Mutex<HashMap<PathBuf, ModuleEvaluation>>,
    /// The CUE package name to evaluate (typically "cuenv").
    package: String,
}

impl CommandExecutor {
    /// Create a new executor with the specified event sender and package name.
    #[must_use]
    pub fn new(event_sender: EventSender, package: String) -> Self {
        Self {
            event_sender,
            local_modules: Mutex::new(HashMap::new()),
            workspace_modules: Mutex::new(HashMap::new()),
            package,
        }
    }

    /// Get the CUE package name used for evaluation.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Get or load a path-local module evaluation (cached by target directory).
    ///
    /// This evaluates only the requested directory (`target_dir`) with
    /// `recursive: false` and does not scan sibling projects.
    ///
    /// # Arguments
    /// * `path` - Directory to evaluate
    ///
    /// # Returns
    /// A `ModuleGuard` that provides direct access to the `ModuleEvaluation`
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The module lock cannot be acquired (poisoned mutex)
    /// - The path cannot be canonicalized
    /// - No CUE module root (cue.mod/) is found starting from the given path
    /// - CUE evaluation fails for the target directory
    pub fn get_module(&self, path: &Path) -> Result<ModuleGuard<'_>> {
        let target_path = path.canonicalize().map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(path.to_path_buf().into_boxed_path()),
            operation: "canonicalize path".to_string(),
        })?;

        let mut guard = self.local_modules.lock().map_err(|_| {
            cuenv_core::Error::configuration("Failed to acquire local module cache lock")
        })?;

        if !guard.contains_key(&target_path) {
            let module = self.evaluate_path_module(&target_path)?;
            guard.insert(target_path.clone(), module);
        }

        Ok(ModuleGuard {
            guard,
            key: target_path,
        })
    }

    /// Discover and load all modules in the current workspace (cached by module root).
    ///
    /// This scans all matching `env.cue` files in the module and evaluates each
    /// directory individually with `recursive: false`, then merges the results.
    ///
    /// Use this only for explicitly workspace-wide commands (for example `sync -A`).
    ///
    /// # Errors
    ///
    /// Returns an error if module discovery/evaluation fails.
    pub fn discover_all_modules(&self, path: &Path) -> Result<ModuleGuard<'_>> {
        let target_path = path.canonicalize().map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(path.to_path_buf().into_boxed_path()),
            operation: "canonicalize path".to_string(),
        })?;

        let module_root = env_file::find_cue_module_root(&target_path).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "No CUE module found (looking for cue.mod/) starting from: {}",
                target_path.display()
            ))
        })?;

        let mut guard = self.workspace_modules.lock().map_err(|_| {
            cuenv_core::Error::configuration("Failed to acquire workspace module cache lock")
        })?;

        if !guard.contains_key(&module_root) {
            let module = self.evaluate_workspace_module(&module_root)?;
            guard.insert(module_root.clone(), module);
        }

        Ok(ModuleGuard {
            guard,
            key: module_root,
        })
    }

    fn evaluate_path_module(&self, target_path: &Path) -> Result<ModuleEvaluation> {
        let module_root = env_file::find_cue_module_root(target_path).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "No CUE module found (looking for cue.mod/) starting from: {}",
                target_path.display()
            ))
        })?;

        let target_rel_path = compute_relative_path(target_path, &module_root);
        let options = ModuleEvalOptions {
            recursive: false,
            with_references: true,
            target_dir: Some(target_path.to_string_lossy().to_string()),
            ..Default::default()
        };

        let raw = cuengine::evaluate_module(&module_root, &self.package, Some(&options))
            .map_err(convert_engine_error)?;

        let mut instances = HashMap::new();
        let mut projects = Vec::new();
        let mut meta = HashMap::new();

        for (path_str, value) in raw.instances {
            let rel_path = if path_str == "." {
                target_rel_path.clone()
            } else {
                path_str
            };
            instances.insert(rel_path, value);
        }

        for project_path in raw.projects {
            let rel_project_path = if project_path == "." {
                target_rel_path.clone()
            } else {
                project_path
            };
            if !projects.contains(&rel_project_path) {
                projects.push(rel_project_path);
            }
        }

        for (meta_key, meta_value) in raw.meta {
            let adjusted_key = adjust_meta_key_path(&meta_key, &target_rel_path);
            meta.insert(adjusted_key, meta_value);
        }

        let references = if meta.is_empty() {
            None
        } else {
            Some(
                meta.into_iter()
                    .filter_map(|(k, v)| v.reference.map(|r| (k, r)))
                    .collect(),
            )
        };

        Ok(ModuleEvaluation::from_raw(
            module_root,
            instances,
            projects,
            references,
        ))
    }

    fn evaluate_workspace_module(&self, module_root: &Path) -> Result<ModuleEvaluation> {
        // For workspace-wide operations, consider all env.cue files regardless of
        // package so we exercise evaluation across the full repository tree.
        // Individual per-directory evaluations still filter by `self.package`.
        let env_cue_dirs =
            cuenv_core::cue::discovery::discover_all_env_cue_directories(module_root);

        if env_cue_dirs.is_empty() {
            return Err(cuenv_core::Error::configuration(format!(
                "No env.cue files found in module: {}",
                module_root.display()
            )));
        }

        let package = &self.package;
        let results: Vec<_> = env_cue_dirs
            .par_iter()
            .map(|dir| {
                let options = ModuleEvalOptions {
                    recursive: false,
                    with_references: true,
                    target_dir: Some(dir.to_string_lossy().to_string()),
                    ..Default::default()
                };
                let dir_rel_path = compute_relative_path(dir, module_root);

                match cuengine::evaluate_module(module_root, package, Some(&options)) {
                    Ok(raw) => Ok((dir_rel_path, raw)),
                    Err(e) => {
                        tracing::warn!(
                            dir = %dir.display(),
                            error = %e,
                            "Failed to evaluate env.cue - skipping directory"
                        );
                        Err((dir.clone(), e))
                    }
                }
            })
            .collect();

        let mut all_instances = HashMap::new();
        let mut all_projects = Vec::new();
        let mut all_meta = HashMap::new();
        let mut eval_errors = Vec::new();

        for result in results {
            match result {
                Ok((dir_rel_path, raw)) => {
                    for (path_str, value) in raw.instances {
                        let rel_path = if path_str == "." {
                            dir_rel_path.clone()
                        } else {
                            path_str
                        };
                        all_instances.insert(rel_path, value);
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

                    for (meta_key, meta_value) in raw.meta {
                        let adjusted_key = adjust_meta_key_path(&meta_key, &dir_rel_path);
                        all_meta.insert(adjusted_key, meta_value);
                    }
                }
                Err((dir, e)) => eval_errors.push((dir, e)),
            }
        }

        if all_instances.is_empty() {
            let error_summary = format_eval_errors(&eval_errors);
            return Err(cuenv_core::Error::configuration(format!(
                "No instances could be evaluated. All directories failed:\n{error_summary}"
            )));
        }

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

        Ok(ModuleEvaluation::from_raw(
            module_root.to_path_buf(),
            all_instances,
            all_projects,
            references,
        ))
    }

    /// Get the module root path if at least one module has been loaded.
    ///
    /// Returns `None` if no module has been loaded yet.
    #[must_use]
    pub fn module_root(&self) -> Option<PathBuf> {
        let local_root = self
            .local_modules
            .lock()
            .ok()
            .and_then(|g| g.values().next().map(|m| m.root.clone()));
        if local_root.is_some() {
            return local_root;
        }
        self.workspace_modules
            .lock()
            .ok()
            .and_then(|g| g.values().next().map(|m| m.root.clone()))
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
        let Ok(target_path) = path.canonicalize() else {
            return false;
        };

        let check = |modules: &HashMap<PathBuf, ModuleEvaluation>| -> Option<bool> {
            modules.values().find_map(|module| {
                let rel_path = relative_path_from_root(&module.root, &target_path);
                module
                    .get(&rel_path)
                    .map(|instance| instance.kind == InstanceKind::Project)
            })
        };

        if let Ok(modules) = self.local_modules.lock()
            && let Some(result) = check(&modules)
        {
            return result;
        }

        self.workspace_modules
            .lock()
            .ok()
            .and_then(|modules| check(&modules))
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
            | Command::ToolsList
            | Command::Build { .. }
            | Command::Up { .. }
            | Command::Down { .. }
            | Command::Logs { .. }
            | Command::Ps { .. }
            | Command::Restart { .. } => Ok(()),
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
