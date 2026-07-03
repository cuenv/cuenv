//! Execution-side extension methods for the [`Task`] DTO.
//!
//! The DTO lives in `cuenv-manifest`; building the concrete process
//! invocation needs core's error type and shell resolution, so it is
//! provided as an extension trait here in the execution engine.

use crate::shell::EffectiveScriptShell;
use crate::{ScriptShell, Task, TaskCommandSpec};
use cuenv_core::{Error, Result};

/// Execution-side extension methods for [`Task`].
pub(crate) trait TaskCommandExt {
    /// Build the executable invocation for this task using the provided
    /// command resolver.
    fn command_spec<F>(&self, resolve_command: F) -> Result<TaskCommandSpec>
    where
        F: FnMut(&str) -> String;
}

impl TaskCommandExt for Task {
    fn command_spec<F>(&self, mut resolve_command: F) -> Result<TaskCommandSpec>
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

fn prepare_script(task: &Task, script: &str, shell: &EffectiveScriptShell) -> Result<String> {
    let Some(shell_options) = task.shell_options else {
        return Ok(script.to_string());
    };

    if !shell.supports_shell_options {
        return Err(Error::configuration(format!(
            "Task uses shellOptions with unsupported script shell '{}'. \
             Use scriptShell 'bash', 'sh', or 'zsh'.",
            shell.display_name
        )));
    }

    if shell_options.pipefail.is_enabled() && !shell.supports_pipefail {
        return Err(Error::configuration(format!(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Shell, ShellOptionToggle, ShellOptions};

    #[test]
    fn test_task_command_spec_uses_script_shell() {
        let task = Task {
            script: Some("echo hello".to_string()),
            script_shell: Some(ScriptShell::Nu),
            ..Default::default()
        };

        let spec = task
            .command_spec(|command| format!("resolved:{command}"))
            .unwrap();

        assert_eq!(spec.program, "resolved:nu");
        assert_eq!(spec.args, vec!["-c".to_string(), "echo hello".to_string()]);
    }

    #[test]
    fn test_task_command_spec_prepends_shell_options() {
        let task = Task {
            script: Some("echo hello".to_string()),
            shell_options: Some(ShellOptions {
                errexit: ShellOptionToggle::Enabled,
                nounset: ShellOptionToggle::Disabled,
                pipefail: ShellOptionToggle::Disabled,
                xtrace: ShellOptionToggle::Enabled,
            }),
            ..Default::default()
        };

        let spec = task.command_spec(str::to_string).unwrap();

        assert_eq!(spec.program, "bash");
        assert_eq!(
            spec.args,
            vec!["-c".to_string(), "set -e -x\necho hello".to_string()]
        );
    }

    #[test]
    fn test_task_command_spec_rejects_pipefail_for_sh() {
        let task = Task {
            script: Some("echo hello".to_string()),
            script_shell: Some(ScriptShell::Sh),
            shell_options: Some(ShellOptions::default()),
            ..Default::default()
        };

        let err = task.command_spec(str::to_string).unwrap_err();

        assert!(
            err.to_string()
                .contains("shellOptions.pipefail with unsupported script shell 'sh'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_task_command_spec_rejects_shell_options_for_unsupported_shell() {
        let task = Task {
            script: Some("console.log('hello')".to_string()),
            script_shell: Some(ScriptShell::Node),
            shell_options: Some(ShellOptions::default()),
            ..Default::default()
        };

        let err = task.command_spec(str::to_string).unwrap_err();

        assert!(
            err.to_string().contains("unsupported script shell 'node'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_task_command_spec_does_not_resolve_empty_command_for_shell_wrapper() {
        let task = Task {
            args: vec!["echo".to_string(), "hello".to_string()],
            shell: Some(Shell {
                command: Some("bash".to_string()),
                flag: Some("-c".to_string()),
            }),
            ..Default::default()
        };

        let mut resolved_commands = Vec::new();
        let spec = task
            .command_spec(|command| {
                resolved_commands.push(command.to_string());
                format!("resolved:{command}")
            })
            .unwrap();

        assert_eq!(resolved_commands, vec!["bash".to_string()]);
        assert_eq!(spec.program, "resolved:bash");
        assert_eq!(spec.args, vec!["-c".to_string(), "echo hello".to_string()]);
    }
}
