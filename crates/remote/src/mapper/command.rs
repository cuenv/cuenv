use crate::reapi::build::bazel::remote::execution::v2 as reapi;
use cuenv_core::tasks::Task;
use crate::RemoteError;
use std::collections::HashMap;

pub struct CommandMapper;

impl CommandMapper {
    pub fn map_task(task: &Task, resolved_env: &HashMap<String, String>) -> Result<reapi::Command, RemoteError> {
        let mut environment_variables: Vec<reapi::command::EnvironmentVariable> = resolved_env.iter()
            .map(|(k, v)| reapi::command::EnvironmentVariable {
                name: k.clone(),
                value: v.clone(),
            })
            .collect();
        
        // REAPI requires environment variables to be sorted by name
        environment_variables.sort_by(|a, b| a.name.cmp(&b.name));

        let arguments = if let Some(shell) = &task.shell {
             let shell_cmd = shell.command.clone().unwrap_or_else(|| "bash".to_string());
             let shell_flag = shell.flag.clone().unwrap_or_else(|| "-c".to_string());
             
             if let Some(script) = &task.script {
                 vec![shell_cmd, shell_flag, script.clone()]
             } else {
                 let mut args = vec![shell_cmd, shell_flag, task.command.clone()];
                 args.extend(task.args.clone());
                 args
             }
        } else {
             if let Some(script) = &task.script {
                 vec!["bash".to_string(), "-c".to_string(), script.clone()]
             } else {
                 let mut args = vec![task.command.clone()];
                 args.extend(task.args.clone());
                 args
             }
        };

        Ok(reapi::Command {
            arguments,
            environment_variables,
            output_files: vec![],
            output_directories: vec![],
            output_paths: task.outputs.clone(),
            platform: None, 
            working_directory: task.directory.clone().unwrap_or_default(),
            output_node_properties: vec![],
            output_directory_format: reapi::command::OutputDirectoryFormat::TreeOnly as i32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::tasks::{Task, Shell};

    #[test]
    fn test_map_simple_command() {
        let task = Task {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            outputs: vec!["out.txt".to_string()],
            ..Default::default()
        };
        let env = HashMap::new();
        
        let command = CommandMapper::map_task(&task, &env).unwrap();
        
        assert_eq!(command.arguments, vec!["echo", "hello"]);
        assert_eq!(command.output_paths, vec!["out.txt"]);
        assert!(command.environment_variables.is_empty());
    }

    #[test]
    fn test_map_script() {
        let task = Task {
            script: Some("echo hello > out.txt".to_string()),
            ..Default::default()
        };
        let env = HashMap::new();
        
        let command = CommandMapper::map_task(&task, &env).unwrap();
        
        assert_eq!(command.arguments, vec!["bash", "-c", "echo hello > out.txt"]);
    }

    #[test]
    fn test_map_env_sorted() {
        let task = Task::default();
        let mut env = HashMap::new();
        env.insert("B".to_string(), "2".to_string());
        env.insert("A".to_string(), "1".to_string());
        
        let command = CommandMapper::map_task(&task, &env).unwrap();
        
        assert_eq!(command.environment_variables.len(), 2);
        assert_eq!(command.environment_variables[0].name, "A");
        assert_eq!(command.environment_variables[1].name, "B");
    }
}
