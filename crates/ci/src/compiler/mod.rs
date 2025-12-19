//! Compiler from cuenv task definitions to IR v1.3
//!
//! Transforms a cuenv `Project` with tasks into an intermediate representation
//! suitable for emitting orchestrator-native CI configurations.

pub mod digest;

use crate::ir::{
    CachePolicy, IntermediateRepresentation, IrValidator, OutputDeclaration, OutputType,
    PurityMode, ResourceRequirements, Runtime, SecretConfig, Task as IrTask, TriggerCondition,
    ValidationError,
};
use cuenv_core::manifest::Project;
use cuenv_core::tasks::{Task, TaskDefinition, TaskGroup};
use std::collections::HashMap;
use thiserror::Error;

/// Compiler errors
#[derive(Debug, Error)]
pub enum CompilerError {
    #[error("Task graph validation failed: {0}")]
    ValidationFailed(String),

    #[error("Task '{0}' uses shell script but IR requires command array")]
    ShellScriptNotSupported(String),

    #[error("Invalid task structure: {0}")]
    InvalidTaskStructure(String),
}

/// Compiler for transforming cuenv tasks to IR
pub struct Compiler {
    /// Project being compiled
    project: Project,

    /// Compiler options
    options: CompilerOptions,
}

/// Compiler configuration options
#[derive(Debug, Clone, Default)]
pub struct CompilerOptions {
    /// Default purity mode for runtimes
    pub purity_mode: PurityMode,

    /// Whether to validate inputs exist at compile time
    pub validate_inputs: bool,

    /// Default cache policy for tasks
    pub default_cache_policy: CachePolicy,
}

impl Compiler {
    /// Create a new compiler for the given project
    pub fn new(project: Project) -> Self {
        Self {
            project,
            options: CompilerOptions::default(),
        }
    }

    /// Create a compiler with custom options
    pub fn with_options(project: Project, options: CompilerOptions) -> Self {
        Self { project, options }
    }

    /// Compile project tasks to IR
    pub fn compile(&self) -> Result<IntermediateRepresentation, CompilerError> {
        let mut ir = IntermediateRepresentation::new(&self.project.name);

        // Set up trigger conditions from CI configuration
        if let Some(ci_config) = &self.project.ci {
            if let Some(first_pipeline) = ci_config.pipelines.first() {
                if let Some(when_condition) = &first_pipeline.when {
                    ir.pipeline.trigger = Some(TriggerCondition {
                        branch: when_condition.branch.as_ref().and_then(|b| match b {
                            cuenv_core::ci::StringOrVec::String(s) => Some(s.clone()),
                            cuenv_core::ci::StringOrVec::Vec(v) => v.first().cloned(),
                        }),
                    });
                }
            }
        }

        // Compile tasks
        self.compile_tasks(&self.project.tasks, &mut ir)?;

        // Validate the IR
        let validator = IrValidator::new(&ir);
        validator.validate().map_err(|errors| {
            let error_messages: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
            CompilerError::ValidationFailed(error_messages.join(", "))
        })?;

        Ok(ir)
    }

    /// Compile task definitions into IR tasks
    fn compile_tasks(
        &self,
        tasks: &HashMap<String, TaskDefinition>,
        ir: &mut IntermediateRepresentation,
    ) -> Result<(), CompilerError> {
        for (name, task_def) in tasks {
            self.compile_task_definition(name, task_def, ir)?;
        }
        Ok(())
    }

    /// Compile a single task definition (handles groups and single tasks)
    fn compile_task_definition(
        &self,
        name: &str,
        task_def: &TaskDefinition,
        ir: &mut IntermediateRepresentation,
    ) -> Result<(), CompilerError> {
        match task_def {
            TaskDefinition::Single(task) => {
                let ir_task = self.compile_single_task(name, task)?;
                ir.tasks.push(ir_task);
            }
            TaskDefinition::Group(group) => {
                self.compile_task_group(name, group, ir)?;
            }
        }
        Ok(())
    }

    /// Compile a task group (sequential or parallel)
    fn compile_task_group(
        &self,
        prefix: &str,
        group: &TaskGroup,
        ir: &mut IntermediateRepresentation,
    ) -> Result<(), CompilerError> {
        match group {
            TaskGroup::Sequential(tasks) => {
                for (idx, task_def) in tasks.iter().enumerate() {
                    let task_name = format!("{}.{}", prefix, idx);
                    self.compile_task_definition(&task_name, task_def, ir)?;
                }
            }
            TaskGroup::Parallel(parallel) => {
                for (name, task_def) in &parallel.tasks {
                    let task_name = format!("{}.{}", prefix, name);
                    self.compile_task_definition(&task_name, task_def, ir)?;
                }
            }
        }
        Ok(())
    }

    /// Compile a single task to IR format
    fn compile_single_task(&self, id: &str, task: &Task) -> Result<IrTask, CompilerError> {
        // Convert command and args to array format
        let command = if !task.command.is_empty() {
            let mut cmd = vec![task.command.clone()];
            cmd.extend(task.args.clone());
            cmd
        } else if let Some(script) = &task.script {
            // For scripts, we need to use shell mode
            // Note: This is a simplified approach; full implementation would
            // need to handle shebang parsing for polyglot scripts
            vec!["/bin/sh".to_string(), "-c".to_string(), script.clone()]
        } else {
            return Err(CompilerError::InvalidTaskStructure(format!(
                "Task '{}' has neither command nor script",
                id
            )));
        };

        // Determine shell mode
        let shell = task.shell.is_some() || task.script.is_some();

        // Convert environment variables (filter out complex JSON values)
        let env: HashMap<String, String> = task
            .env
            .iter()
            .filter_map(|(k, v)| {
                if let Some(s) = v.as_str() {
                    Some((k.clone(), s.to_string()))
                } else {
                    // Skip complex values for now (would need secret resolution)
                    None
                }
            })
            .collect();

        // Extract secrets (simplified - would integrate with secret resolver)
        let secrets: HashMap<String, SecretConfig> = HashMap::new();

        // Convert inputs (path globs only for now)
        let inputs: Vec<String> = task.iter_path_inputs().cloned().collect();

        // Convert outputs
        let outputs: Vec<OutputDeclaration> = task
            .outputs
            .iter()
            .map(|path| OutputDeclaration {
                path: path.clone(),
                output_type: OutputType::Cas, // Default to CAS
            })
            .collect();

        // Determine cache policy
        let cache_policy = if task.labels.contains(&"deployment".to_string()) {
            CachePolicy::Disabled
        } else {
            self.options.default_cache_policy
        };

        // Determine if this is a deployment task
        let deployment = task.labels.contains(&"deployment".to_string());

        Ok(IrTask {
            id: id.to_string(),
            runtime: None, // Would be set based on Nix flake configuration
            command,
            shell,
            env,
            secrets,
            resources: None, // Would extract from task metadata if available
            concurrency_group: None,
            inputs,
            outputs,
            depends_on: task.depends_on.clone(),
            cache_policy,
            deployment,
            manual_approval: false, // Would come from task metadata
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::tasks::Task;

    #[test]
    fn test_compile_simple_task() {
        let mut project = Project::new("test-project");
        project.tasks.insert(
            "build".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo".to_string(),
                args: vec!["build".to_string()],
                inputs: vec![cuenv_core::tasks::Input::Path("src/**/*.rs".to_string())],
                outputs: vec!["target/debug/binary".to_string()],
                ..Default::default()
            })),
        );

        let compiler = Compiler::new(project);
        let ir = compiler.compile().unwrap();

        assert_eq!(ir.version, "1.3");
        assert_eq!(ir.pipeline.name, "test-project");
        assert_eq!(ir.tasks.len(), 1);
        assert_eq!(ir.tasks[0].id, "build");
        assert_eq!(ir.tasks[0].command, vec!["cargo", "build"]);
        assert_eq!(ir.tasks[0].inputs, vec!["src/**/*.rs"]);
    }

    #[test]
    fn test_compile_task_with_dependencies() {
        let mut project = Project::new("test-project");

        project.tasks.insert(
            "test".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo".to_string(),
                args: vec!["test".to_string()],
                depends_on: vec!["build".to_string()],
                ..Default::default()
            })),
        );

        project.tasks.insert(
            "build".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo".to_string(),
                args: vec!["build".to_string()],
                ..Default::default()
            })),
        );

        let compiler = Compiler::new(project);
        let ir = compiler.compile().unwrap();

        assert_eq!(ir.tasks.len(), 2);

        let test_task = ir.tasks.iter().find(|t| t.id == "test").unwrap();
        assert_eq!(test_task.depends_on, vec!["build"]);
    }

    #[test]
    fn test_compile_deployment_task() {
        let mut project = Project::new("test-project");

        project.tasks.insert(
            "deploy".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "kubectl".to_string(),
                args: vec!["apply".to_string()],
                labels: vec!["deployment".to_string()],
                ..Default::default()
            })),
        );

        let compiler = Compiler::new(project);
        let ir = compiler.compile().unwrap();

        assert_eq!(ir.tasks.len(), 1);
        assert_eq!(ir.tasks[0].deployment, true);
        assert_eq!(ir.tasks[0].cache_policy, CachePolicy::Disabled);
    }

    #[test]
    fn test_compile_script_task() {
        let mut project = Project::new("test-project");

        project.tasks.insert(
            "script-task".to_string(),
            TaskDefinition::Single(Box::new(Task {
                script: Some("echo 'Running script'\nls -la".to_string()),
                ..Default::default()
            })),
        );

        let compiler = Compiler::new(project);
        let ir = compiler.compile().unwrap();

        assert_eq!(ir.tasks.len(), 1);
        assert_eq!(ir.tasks[0].shell, true);
        assert_eq!(ir.tasks[0].command[0], "/bin/sh");
        assert_eq!(ir.tasks[0].command[1], "-c");
    }
}
