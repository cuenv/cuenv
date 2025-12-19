//! Mapper from cuenv Task to REAPI Command

use crate::config::SecretsMode;
use crate::error::Result;
use crate::merkle::Digest;
use crate::reapi::{
    Command as ReapiCommand, Platform, command::EnvironmentVariable, platform::Property,
};
use cuenv_core::environment::Environment;
use cuenv_core::tasks::Task;
use prost::Message;
use std::collections::HashMap;

/// Result of mapping a task to an REAPI Command
#[derive(Debug)]
pub struct MappedCommand {
    /// The REAPI Command proto
    pub command: ReapiCommand,
    /// Serialized bytes of the command
    pub command_bytes: Vec<u8>,
    /// Digest of the serialized command
    pub command_digest: Digest,
    /// Secrets to be passed via headers (not in command)
    pub secrets_headers: HashMap<String, String>,
}

/// Mapper for converting cuenv Task to REAPI Command
pub struct CommandMapper;

impl CommandMapper {
    /// Map a cuenv Task to an REAPI Command
    ///
    /// Returns the command proto, its serialized bytes, digest, and any secrets for headers.
    pub fn map_task(
        task: &Task,
        environment: &Environment,
        secrets_mode: &SecretsMode,
    ) -> Result<MappedCommand> {
        // Build arguments
        let arguments = Self::build_arguments(task);

        // Build environment variables (may extract secrets for headers)
        let (env_vars, secrets_headers) =
            Self::build_environment_variables(environment, secrets_mode);

        // Platform properties
        let platform = Self::build_platform(task);

        // Output paths from task outputs
        let output_paths = Self::build_output_paths(task);

        // Create the command proto
        #[allow(deprecated)]
        let command = ReapiCommand {
            arguments,
            environment_variables: env_vars,
            output_paths,
            output_files: vec![],       // deprecated, use output_paths
            output_directories: vec![], // deprecated, use output_paths
            output_node_properties: vec![],
            platform: Some(platform), // deprecated but still needed for compatibility
            working_directory: String::new(), // Tasks run in root of input tree
            output_directory_format: 0, // TREE_ONLY
        };

        // Serialize and compute digest
        let command_bytes = command.encode_to_vec();
        let command_digest = Digest::from_bytes(&command_bytes);

        Ok(MappedCommand {
            command,
            command_bytes,
            command_digest,
            secrets_headers,
        })
    }

    /// Build command arguments from task
    fn build_arguments(task: &Task) -> Vec<String> {
        if let Some(ref shell) = task.shell {
            // Shell mode: [shell, flag, script]
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
        }
    }

    /// Build environment variables from environment
    ///
    /// For SecretsMode::Headers, secrets are extracted and returned separately
    /// to be passed via BuildBuddy remote headers.
    fn build_environment_variables(
        environment: &Environment,
        secrets_mode: &SecretsMode,
    ) -> (Vec<EnvironmentVariable>, HashMap<String, String>) {
        let mut env_vars = Vec::new();
        let mut secrets_headers = HashMap::new();

        for (k, v) in &environment.vars {
            match secrets_mode {
                SecretsMode::Inline => {
                    // All variables go into the command (less secure)
                    env_vars.push(EnvironmentVariable {
                        name: k.clone(),
                        value: v.clone(),
                    });
                }
                SecretsMode::Headers => {
                    // Check if this looks like a secret (starts with secret marker or contains key patterns)
                    // Secrets are passed via BuildBuddy headers, not in the command
                    if Self::is_secret_variable(k) {
                        secrets_headers.insert(k.clone(), v.clone());
                    } else {
                        env_vars.push(EnvironmentVariable {
                            name: k.clone(),
                            value: v.clone(),
                        });
                    }
                }
            }
        }

        // Sort for determinism
        env_vars.sort_by(|a, b| a.name.cmp(&b.name));

        (env_vars, secrets_headers)
    }

    /// Check if a variable name looks like a secret
    fn is_secret_variable(name: &str) -> bool {
        let name_upper = name.to_uppercase();
        name_upper.contains("SECRET")
            || name_upper.contains("PASSWORD")
            || name_upper.contains("TOKEN")
            || name_upper.contains("API_KEY")
            || name_upper.contains("PRIVATE_KEY")
            || name_upper.contains("CREDENTIALS")
    }

    /// Build platform properties for the execution environment
    fn build_platform(task: &Task) -> Platform {
        let mut properties = vec![Property {
            name: "OSFamily".to_string(),
            value: "Linux".to_string(),
        }];

        // Add container image if specified in dagger config
        if let Some(ref dagger) = task.dagger {
            if let Some(ref image) = dagger.image {
                properties.push(Property {
                    name: "container-image".to_string(),
                    value: format!("docker://{}", image),
                });
            }
        }

        Platform { properties }
    }

    /// Build output paths from task outputs
    fn build_output_paths(task: &Task) -> Vec<String> {
        // Task outputs specify files that should be captured after execution
        task.outputs.clone()
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
        let mapped = CommandMapper::map_task(&task, &env, &SecretsMode::Inline).unwrap();

        assert_eq!(mapped.command.arguments, vec!["echo", "hello"]);
        assert!(!mapped.command_bytes.is_empty());
        assert_eq!(mapped.command_digest.hash.len(), 64);
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
        let mapped = CommandMapper::map_task(&task, &env, &SecretsMode::Inline).unwrap();

        assert_eq!(mapped.command.arguments.len(), 3);
        assert_eq!(mapped.command.arguments[0], "bash");
        assert_eq!(mapped.command.arguments[1], "-c");
        assert_eq!(mapped.command.arguments[2], "echo hello && echo world");
    }

    #[test]
    fn test_secrets_mode_headers() {
        let task = Task {
            command: "echo".to_string(),
            ..Default::default()
        };

        let mut env = Environment::default();
        env.vars
            .insert("API_TOKEN".to_string(), "secret123".to_string());
        env.vars
            .insert("NORMAL_VAR".to_string(), "value".to_string());

        let mapped = CommandMapper::map_task(&task, &env, &SecretsMode::Headers).unwrap();

        // Secret should be in headers, not in command
        assert!(mapped.secrets_headers.contains_key("API_TOKEN"));
        assert!(
            !mapped
                .command
                .environment_variables
                .iter()
                .any(|v| v.name == "API_TOKEN")
        );

        // Normal var should be in command
        assert!(
            mapped
                .command
                .environment_variables
                .iter()
                .any(|v| v.name == "NORMAL_VAR")
        );
    }

    #[test]
    fn test_deterministic_env_ordering() {
        let task = Task {
            command: "echo".to_string(),
            ..Default::default()
        };

        let mut env = Environment::default();
        env.vars.insert("Z_VAR".to_string(), "z".to_string());
        env.vars.insert("A_VAR".to_string(), "a".to_string());
        env.vars.insert("M_VAR".to_string(), "m".to_string());

        let mapped = CommandMapper::map_task(&task, &env, &SecretsMode::Inline).unwrap();

        // Environment variables should be sorted by name
        let names: Vec<_> = mapped
            .command
            .environment_variables
            .iter()
            .map(|v| &v.name)
            .collect();
        assert_eq!(names, vec!["A_VAR", "M_VAR", "Z_VAR"]);
    }
}
