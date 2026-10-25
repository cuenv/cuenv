/// Build container images defined in CUE configuration.
pub mod build;
/// Interactive changeset picker for selecting changes to include.
pub mod changeset_picker;
/// CI pipeline integration and generation commands.
pub mod ci;
mod command;
mod dispatch;
/// Request shutdown of the active service session.
pub mod down;
/// Environment management commands (print, load, status, etc.).
pub mod env;
/// CUE module and env.cue file discovery utilities.
pub mod env_file;
/// Execute arbitrary commands within the cuenv environment.
pub mod exec;
mod executor;
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
/// Infrastructure as code through Terraform provider plugins.
pub mod infrastructure;
/// View service logs.
pub mod logs;
mod module_evaluation;
mod module_utils;
/// List running services and their status.
pub mod ps;
/// Release management commands (prepare, version, publish, binaries).
pub mod release;
/// Restart one or more services.
pub mod restart;
/// CUE schema dependency compatibility warnings.
pub mod schema_compat;
/// Secrets provider setup and management.
pub mod secrets;
/// Hidden process-babysitter subcommand (`cuenv __supervise`).
#[cfg(unix)]
pub mod supervise;
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

pub use self::command::Command;
pub use self::executor::CommandExecutor;
pub use module_utils::convert_engine_error;
pub use module_utils::{ModuleGuard, relative_path_from_root};

#[cfg(test)]
mod tests;
