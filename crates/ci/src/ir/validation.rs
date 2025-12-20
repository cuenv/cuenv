//! IR Validation
//!
//! Validates IR documents for correctness according to PRD v1.3 rules.

use super::schema::{CachePolicy, IntermediateRepresentation, Task};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

/// Validation errors for IR documents
#[derive(Debug, Error, PartialEq)]
pub enum ValidationError {
    #[error("Task graph contains cycle: {0}")]
    CyclicDependency(String),

    #[error("Task '{task}' depends on non-existent task '{dependency}'")]
    MissingDependency { task: String, dependency: String },

    #[error("Task '{task}' references non-existent runtime '{runtime}'")]
    MissingRuntime { task: String, runtime: String },

    #[error("Deployment task '{deployment}' has non-deployment dependent '{dependent}'")]
    InvalidDeploymentDependency {
        deployment: String,
        dependent: String,
    },

    #[error("Task '{task}' has shell=false with string command (must be array)")]
    InvalidShellCommand { task: String },

    #[error("Task '{task}' has empty command")]
    EmptyCommand { task: String },

    #[error("Deployment task '{task}' has cache_policy={policy:?} (must be disabled)")]
    InvalidDeploymentCachePolicy { task: String, policy: CachePolicy },

    #[error("Task '{task}' declares input '{input}' that does not exist at compile time")]
    MissingInput { task: String, input: String },
}

/// Validator for IR documents
pub struct IrValidator<'a> {
    ir: &'a IntermediateRepresentation,
}

impl<'a> IrValidator<'a> {
    /// Create a new validator for the given IR
    #[must_use]
    pub fn new(ir: &'a IntermediateRepresentation) -> Self {
        Self { ir }
    }

    /// Validate the entire IR document
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();

        // Build task index
        let task_index: HashMap<&str, &Task> =
            self.ir.tasks.iter().map(|t| (t.id.as_str(), t)).collect();

        // Build runtime index
        let runtime_ids: HashSet<&str> = self.ir.runtimes.iter().map(|r| r.id.as_str()).collect();

        for task in &self.ir.tasks {
            // Validate command
            if let Err(e) = self.validate_command(task) {
                errors.push(e);
            }

            // Validate runtime reference
            if let Some(runtime) = &task.runtime
                && !runtime_ids.contains(runtime.as_str())
            {
                errors.push(ValidationError::MissingRuntime {
                    task: task.id.clone(),
                    runtime: runtime.clone(),
                });
            }

            // Validate dependencies exist
            for dep in &task.depends_on {
                if !task_index.contains_key(dep.as_str()) {
                    errors.push(ValidationError::MissingDependency {
                        task: task.id.clone(),
                        dependency: dep.clone(),
                    });
                }
            }

            // Validate deployment task constraints
            if task.deployment && task.cache_policy != CachePolicy::Disabled {
                errors.push(ValidationError::InvalidDeploymentCachePolicy {
                    task: task.id.clone(),
                    policy: task.cache_policy,
                });
            }
        }

        // Validate no cycles in task graph
        if let Err(e) = self.validate_no_cycles(&task_index) {
            errors.push(e);
        }

        // Validate deployment dependencies
        if let Err(mut e) = self.validate_deployment_dependencies(&task_index) {
            errors.append(&mut e);
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Validate task command is well-formed
    fn validate_command(&self, task: &Task) -> Result<(), ValidationError> {
        if task.command.is_empty() {
            return Err(ValidationError::EmptyCommand {
                task: task.id.clone(),
            });
        }

        // If shell is false, command must be properly structured for direct execve
        // (already an array, so this is satisfied by the type system)

        Ok(())
    }

    /// Validate task graph has no cycles using DFS
    fn validate_no_cycles(&self, task_index: &HashMap<&str, &Task>) -> Result<(), ValidationError> {
        let mut visited = HashSet::new();
        let mut rec_stack = HashSet::new();

        for task in &self.ir.tasks {
            if !visited.contains(task.id.as_str())
                && let Some(cycle) =
                    self.detect_cycle(task.id.as_str(), task_index, &mut visited, &mut rec_stack)
            {
                return Err(ValidationError::CyclicDependency(cycle));
            }
        }

        Ok(())
    }

    /// Detect cycles using DFS, returns path if cycle found
    fn detect_cycle(
        &self,
        task_id: &str,
        task_index: &HashMap<&str, &Task>,
        visited: &mut HashSet<String>,
        rec_stack: &mut HashSet<String>,
    ) -> Option<String> {
        visited.insert(task_id.to_string());
        rec_stack.insert(task_id.to_string());

        if let Some(task) = task_index.get(task_id) {
            for dep in &task.depends_on {
                if !visited.contains(dep.as_str()) {
                    if let Some(cycle) = self.detect_cycle(dep, task_index, visited, rec_stack) {
                        return Some(format!("{task_id} -> {cycle}"));
                    }
                } else if rec_stack.contains(dep.as_str()) {
                    // Found a cycle
                    return Some(format!("{task_id} -> {dep}"));
                }
            }
        }

        rec_stack.remove(task_id);
        None
    }

    /// Validate deployment task dependency rules
    fn validate_deployment_dependencies(
        &self,
        _task_index: &HashMap<&str, &Task>,
    ) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();

        // Find all deployment tasks
        let deployment_tasks: HashSet<&str> = self
            .ir
            .tasks
            .iter()
            .filter(|t| t.deployment)
            .map(|t| t.id.as_str())
            .collect();

        // Check that no non-deployment task depends on a deployment task
        for task in &self.ir.tasks {
            if !task.deployment {
                for dep in &task.depends_on {
                    if deployment_tasks.contains(dep.as_str()) {
                        errors.push(ValidationError::InvalidDeploymentDependency {
                            deployment: dep.clone(),
                            dependent: task.id.clone(),
                        });
                    }
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{PurityMode, Runtime};

    fn create_test_task(id: &str, depends_on: Vec<&str>) -> Task {
        Task {
            id: id.to_string(),
            runtime: None,
            command: vec!["echo".to_string()],
            shell: false,
            env: HashMap::new(),
            secrets: HashMap::new(),
            resources: None,
            concurrency_group: None,
            inputs: vec![],
            outputs: vec![],
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
            cache_policy: CachePolicy::Normal,
            deployment: false,
            manual_approval: false,
        }
    }

    #[test]
    fn test_valid_ir() {
        let mut ir = IntermediateRepresentation::new("test");
        ir.tasks.push(create_test_task("task1", vec![]));
        ir.tasks.push(create_test_task("task2", vec!["task1"]));

        let validator = IrValidator::new(&ir);
        assert!(validator.validate().is_ok());
    }

    #[test]
    fn test_cyclic_dependency() {
        let mut ir = IntermediateRepresentation::new("test");
        ir.tasks.push(create_test_task("task1", vec!["task2"]));
        ir.tasks.push(create_test_task("task2", vec!["task1"]));

        let validator = IrValidator::new(&ir);
        let result = validator.validate();
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::CyclicDependency(_)))
        );
    }

    #[test]
    fn test_missing_dependency() {
        let mut ir = IntermediateRepresentation::new("test");
        ir.tasks
            .push(create_test_task("task1", vec!["nonexistent"]));

        let validator = IrValidator::new(&ir);
        let result = validator.validate();
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0],
            ValidationError::MissingDependency { .. }
        ));
    }

    #[test]
    fn test_deployment_task_must_have_disabled_cache() {
        let mut ir = IntermediateRepresentation::new("test");
        let mut deploy_task = create_test_task("deploy", vec![]);
        deploy_task.deployment = true;
        deploy_task.cache_policy = CachePolicy::Normal; // Invalid!
        ir.tasks.push(deploy_task);

        let validator = IrValidator::new(&ir);
        let result = validator.validate();
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::InvalidDeploymentCachePolicy { .. }))
        );
    }

    #[test]
    fn test_deployment_task_valid_with_disabled_cache() {
        let mut ir = IntermediateRepresentation::new("test");
        let mut deploy_task = create_test_task("deploy", vec![]);
        deploy_task.deployment = true;
        deploy_task.cache_policy = CachePolicy::Disabled;
        ir.tasks.push(deploy_task);

        let validator = IrValidator::new(&ir);
        assert!(validator.validate().is_ok());
    }

    #[test]
    fn test_non_deployment_cannot_depend_on_deployment() {
        let mut ir = IntermediateRepresentation::new("test");

        let mut deploy_task = create_test_task("deploy", vec![]);
        deploy_task.deployment = true;
        deploy_task.cache_policy = CachePolicy::Disabled;
        ir.tasks.push(deploy_task);

        let build_task = create_test_task("build", vec!["deploy"]);
        ir.tasks.push(build_task);

        let validator = IrValidator::new(&ir);
        let result = validator.validate();
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::InvalidDeploymentDependency { .. }))
        );
    }

    #[test]
    fn test_deployment_can_depend_on_deployment() {
        let mut ir = IntermediateRepresentation::new("test");

        let mut deploy1 = create_test_task("deploy-staging", vec![]);
        deploy1.deployment = true;
        deploy1.cache_policy = CachePolicy::Disabled;
        ir.tasks.push(deploy1);

        let mut deploy2 = create_test_task("deploy-prod", vec!["deploy-staging"]);
        deploy2.deployment = true;
        deploy2.cache_policy = CachePolicy::Disabled;
        ir.tasks.push(deploy2);

        let validator = IrValidator::new(&ir);
        assert!(validator.validate().is_ok());
    }

    #[test]
    fn test_empty_command() {
        let mut ir = IntermediateRepresentation::new("test");
        let mut task = create_test_task("task1", vec![]);
        task.command = vec![];
        ir.tasks.push(task);

        let validator = IrValidator::new(&ir);
        let result = validator.validate();
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::EmptyCommand { .. }))
        );
    }

    #[test]
    fn test_missing_runtime() {
        let mut ir = IntermediateRepresentation::new("test");
        let mut task = create_test_task("task1", vec![]);
        task.runtime = Some("nonexistent".to_string());
        ir.tasks.push(task);

        let validator = IrValidator::new(&ir);
        let result = validator.validate();
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::MissingRuntime { .. }))
        );
    }

    #[test]
    fn test_valid_runtime_reference() {
        let mut ir = IntermediateRepresentation::new("test");
        ir.runtimes.push(Runtime {
            id: "nix".to_string(),
            flake: "github:NixOS/nixpkgs/nixos-unstable".to_string(),
            output: "devShells.x86_64-linux.default".to_string(),
            system: "x86_64-linux".to_string(),
            digest: "sha256:abc".to_string(),
            purity: PurityMode::Strict,
        });

        let mut task = create_test_task("task1", vec![]);
        task.runtime = Some("nix".to_string());
        ir.tasks.push(task);

        let validator = IrValidator::new(&ir);
        assert!(validator.validate().is_ok());
    }
}
