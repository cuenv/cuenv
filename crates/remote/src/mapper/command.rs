//! Mapper from cuenv Task to REAPI Command

use crate::config::SecretsMode;
use crate::error::Result;
use cuenv_core::environment::Environment;
use cuenv_core::tasks::Task;
use std::collections::HashMap;

/// Placeholder for REAPI Command proto
///
/// Phase 2: Replace with actual generated type from protos
#[derive(Debug, Clone)]
pub struct Command {
    pub arguments: Vec<String>,
    pub environment_variables: Vec<EnvironmentVariable>,
    pub output_paths: Vec<String>,
    pub platform: Option<Platform>,
}

#[derive(Debug, Clone)]
pub struct EnvironmentVariable {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct Platform {
    pub properties: Vec<Property>,
}

#[derive(Debug, Clone)]
pub struct Property {
    pub name: String,
    pub value: String,
}

/// Mapper for converting cuenv Task to REAPI Command
pub struct CommandMapper;

impl CommandMapper {
    /// Map a cuenv Task to an REAPI Command
    ///
    /// Phase 3: Implement full mapping including:
    /// - Command arguments (handle shell mode)
    /// - Environment variables
    /// - Platform properties
    /// - Output paths
    pub fn map_task(
        task: &Task,
        environment: &Environment,
        _secrets_mode: &SecretsMode,
    ) -> Result<(Command, HashMap<String, String>)> {
        // Build arguments
        let arguments = if task.shell.is_some() {
            // Shell mode: [shell, flag, script]
            let shell = task.shell.as_ref().unwrap();
            let shell_cmd = shell.command.as_deref().unwrap_or("bash");
            let flag = shell.flag.as_deref().unwrap_or("-c");
            vec![
                shell_cmd.to_string(),
                flag.to_string(),
                task.command.clone(),
            ]
        } else {
            // Direct mode: [command, ...args]
            let mut args = vec![task.command.clone()];
            args.extend(task.args.clone());
            args
        };

        // Build environment variables
        // Phase 3: Handle secret resolution here
        // For SecretsMode::Inline, secrets are resolved and included
        // For SecretsMode::Headers, secrets are extracted and returned separately
        let mut env_vars = Vec::new();
        let secrets_for_headers = HashMap::new();

        for (k, v) in &environment.vars {
            env_vars.push(EnvironmentVariable {
                name: k.clone(),
                value: v.clone(),
            });
        }

        // Platform properties
        // Phase 3: Add OS, arch, and any custom platform requirements
        let platform = Some(Platform {
            properties: vec![
                Property {
                    name: "OSFamily".to_string(),
                    value: "Linux".to_string(),
                },
                Property {
                    name: "container-image".to_string(),
                    value: "docker://alpine:latest".to_string(),
                },
            ],
        });

        // Output paths - Phase 3: extract from task.outputs
        let output_paths = Vec::new();

        let command = Command {
            arguments,
            environment_variables: env_vars,
            output_paths,
            platform,
        };

        Ok((command, secrets_for_headers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_simple_task() {
        let task = Task {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            ..Default::default()
        };

        let env = Environment::default();
        let (command, _) = CommandMapper::map_task(&task, &env, &SecretsMode::Inline).unwrap();

        assert_eq!(command.arguments, vec!["echo", "hello"]);
    }

    #[test]
    fn test_map_shell_task() {
        let task = Task {
            command: "echo hello && echo world".to_string(),
            shell: Some(cuenv_core::tasks::Shell {
                command: Some("bash".to_string()),
                flag: Some("-c".to_string()),
            }),
            ..Default::default()
        };

        let env = Environment::default();
        let (command, _) = CommandMapper::map_task(&task, &env, &SecretsMode::Inline).unwrap();

        assert_eq!(command.arguments.len(), 3);
        assert_eq!(command.arguments[0], "bash");
        assert_eq!(command.arguments[1], "-c");
        assert_eq!(command.arguments[2], "echo hello && echo world");
    }
}
