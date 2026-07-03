//! Execution-side shell types.
//!
//! The shell DTOs (`Shell`, `ScriptShell`, `ShellOptions`) live in
//! `cuenv-manifest`; this module holds the resolved invocation types used
//! by the executor.

/// A fully-resolved process invocation for a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskCommandSpec {
    /// Executable path or command name to spawn.
    pub program: String,
    /// Command-line arguments for the program.
    pub args: Vec<String>,
}

/// A script shell resolved to its concrete command, flag, and capabilities.
#[derive(Debug, Clone)]
pub(super) struct EffectiveScriptShell {
    pub(super) command: String,
    pub(super) flag: String,
    pub(super) display_name: String,
    pub(super) supports_shell_options: bool,
    pub(super) supports_pipefail: bool,
}
