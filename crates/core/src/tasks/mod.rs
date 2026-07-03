//! Task execution and management module
//!
//! The task DTO types (`Task`, `TaskGroup`, `TaskNode`, `Tasks`, ...) live
//! in `cuenv-manifest` and are re-exported here; this module owns the
//! execution engine: graph building, scheduling, process management,
//! caching, and command resolution.

pub mod backend;
pub mod cache;
pub mod captures;
mod command;
pub(crate) mod env;
pub mod error;
pub mod executor;
pub mod graph;
pub mod graph_walk;
pub mod index;
pub mod output_refs;
mod process;
pub mod process_registry;
mod result;
mod shell;
mod workspace;

// Re-export the task DTOs from the leaf manifest crate.
pub use cuenv_manifest::tasks::*;

pub use error::TaskError;

// Re-export executor and graph modules
pub use backend::{
    BackendFactory, HostBackend, TaskBackend, TaskExecutionContext, create_backend,
    create_backend_with_factory, should_use_dagger,
};
pub use executor::*;
pub use graph::*;
pub use index::{IndexedTask, TaskIndex, TaskPath, WorkspaceTask};
pub use output_refs::{
    OutputRefResolver, TaskOutputField, TaskOutputRef, has_output_refs, process_output_refs,
};
pub use process_registry::global_registry;
pub(crate) use shell::TaskCommandSpec;

use shell::EffectiveScriptShell;
use std::path::Path;

/// Execution-side extension methods for [`Task`].
///
/// The DTO lives in `cuenv-manifest`; building the concrete process
/// invocation needs core's error type and shell resolution, so it is
/// provided as an extension trait.
pub(crate) trait TaskCommandExt {
    /// Build the executable invocation for this task using the provided
    /// command resolver.
    fn command_spec<F>(&self, resolve_command: F) -> crate::Result<TaskCommandSpec>
    where
        F: FnMut(&str) -> String;
}

impl TaskCommandExt for Task {
    fn command_spec<F>(&self, mut resolve_command: F) -> crate::Result<TaskCommandSpec>
    where
        F: FnMut(&str) -> String,
    {
        if let Some(script) = &self.script {
            let shell = effective_script_shell(self);
            let script = prepare_script(self, script, &shell)?;

            return Ok(TaskCommandSpec {
                program: resolve_command(&shell.command),
                args: vec![shell.flag, script],
            });
        }

        if let Some(shell) = &self.shell
            && let (Some(shell_command), Some(shell_flag)) = (&shell.command, &shell.flag)
        {
            let full_command = if self.command.is_empty() {
                self.args.join(" ")
            } else if self.args.is_empty() {
                resolve_command(&self.command)
            } else {
                let resolved_command = resolve_command(&self.command);
                format!("{} {}", resolved_command, self.args.join(" "))
            };

            return Ok(TaskCommandSpec {
                program: resolve_command(shell_command),
                args: vec![shell_flag.clone(), full_command],
            });
        }

        Ok(TaskCommandSpec {
            program: resolve_command(&self.command),
            args: self.args.clone(),
        })
    }
}

fn effective_script_shell(task: &Task) -> EffectiveScriptShell {
    if let Some(script_shell) = task.script_shell {
        let (command, flag) = script_shell.command_and_flag();

        return EffectiveScriptShell {
            command: command.to_string(),
            flag: flag.to_string(),
            display_name: command.to_string(),
            supports_shell_options: script_shell.supports_shell_options(),
            supports_pipefail: script_shell.supports_pipefail(),
        };
    }

    if let Some(shell) = &task.shell {
        let command = shell.command.clone().unwrap_or_else(|| "bash".to_string());
        let flag = shell.flag.clone().unwrap_or_else(|| "-c".to_string());
        let (supports_shell_options, supports_pipefail) = ScriptShell::from_command(&command)
            .map(|script_shell| {
                (
                    script_shell.supports_shell_options(),
                    script_shell.supports_pipefail(),
                )
            })
            .unwrap_or((false, false));

        return EffectiveScriptShell {
            display_name: command.clone(),
            command,
            flag,
            supports_shell_options,
            supports_pipefail,
        };
    }

    let default_shell = ScriptShell::default();
    let (command, flag) = default_shell.command_and_flag();

    EffectiveScriptShell {
        command: command.to_string(),
        flag: flag.to_string(),
        display_name: command.to_string(),
        supports_shell_options: default_shell.supports_shell_options(),
        supports_pipefail: default_shell.supports_pipefail(),
    }
}

fn prepare_script(
    task: &Task,
    script: &str,
    shell: &EffectiveScriptShell,
) -> crate::Result<String> {
    let Some(shell_options) = task.shell_options else {
        return Ok(script.to_string());
    };

    if !shell.supports_shell_options {
        return Err(crate::Error::configuration(format!(
            "Task uses shellOptions with unsupported script shell '{}'. \
             Use scriptShell 'bash', 'sh', or 'zsh'.",
            shell.display_name
        )));
    }

    if shell_options.pipefail.is_enabled() && !shell.supports_pipefail {
        return Err(crate::Error::configuration(format!(
            "Task uses shellOptions.pipefail with unsupported script shell '{}'. \
             Disable pipefail or use scriptShell 'bash' or 'zsh'.",
            shell.display_name
        )));
    }

    let set_commands = shell_options.to_set_commands();
    if set_commands.is_empty() {
        return Ok(script.to_string());
    }

    Ok(format!("{set_commands}{script}"))
}

impl crate::AffectedBy for Task {
    /// Returns true if this task is affected by the given file changes.
    ///
    /// # Behavior
    ///
    /// - Tasks with NO inputs are always considered affected (we can't determine what affects them)
    /// - Tasks with inputs are affected if any input pattern matches changed files
    fn is_affected_by(&self, changed_files: &[std::path::PathBuf], project_root: &Path) -> bool {
        let inputs: Vec<_> = self.iter_path_inputs().collect();

        // No inputs = always affected (we can't determine what affects it)
        if inputs.is_empty() {
            return true;
        }

        // Check if any input pattern matches any changed file
        inputs
            .iter()
            .any(|pattern| crate::matches_pattern(changed_files, project_root, pattern))
    }

    fn input_patterns(&self) -> Vec<&str> {
        self.iter_path_inputs().map(String::as_str).collect()
    }
}

impl crate::AffectedBy for TaskGroup {
    /// A group is affected if ANY of its subtasks are affected.
    fn is_affected_by(&self, changed_files: &[std::path::PathBuf], project_root: &Path) -> bool {
        self.children
            .values()
            .any(|node| node.is_affected_by(changed_files, project_root))
    }

    fn input_patterns(&self) -> Vec<&str> {
        self.children
            .values()
            .flat_map(|node| crate::AffectedBy::input_patterns(node))
            .collect()
    }
}

impl crate::AffectedBy for TaskNode {
    fn is_affected_by(&self, changed_files: &[std::path::PathBuf], project_root: &Path) -> bool {
        match self {
            Self::Task(task) => task.is_affected_by(changed_files, project_root),
            Self::Group(group) => group.is_affected_by(changed_files, project_root),
            Self::Sequence(seq) => seq
                .iter()
                .any(|node| node.is_affected_by(changed_files, project_root)),
        }
    }

    fn input_patterns(&self) -> Vec<&str> {
        match self {
            Self::Task(task) => crate::AffectedBy::input_patterns(task.as_ref()),
            Self::Group(group) => crate::AffectedBy::input_patterns(group),
            Self::Sequence(seq) => seq
                .iter()
                .flat_map(crate::AffectedBy::input_patterns)
                .collect(),
        }
    }
}

#[cfg(test)]
#[path = "tasks_tests.rs"]
mod tests;
