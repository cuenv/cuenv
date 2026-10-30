//! Lowering task nodes (tasks, groups and sequences) to IR tasks.

use super::{Compiler, CompilerError};
use crate::ir::{
    ArtifactDownload, CachePolicy, IntermediateRepresentation, OutputDeclaration, OutputType,
    SecretConfig, Task as IrTask,
};
use cuenv_core::tasks::{Task, TaskGroup, TaskNode};
use cuenv_task_exec::TaskCommandExt as _;
use std::collections::{BTreeMap, BTreeSet, HashMap};

impl Compiler {
    /// Compile a single task by name to IR
    ///
    /// This method handles both single tasks and task groups, compiling them
    /// into an IR representation that can be executed.
    ///
    /// # Errors
    ///
    /// Returns `CompilerError` if the task is not found or compilation fails.
    pub fn compile_task(
        &self,
        task_name: &str,
    ) -> Result<IntermediateRepresentation, CompilerError> {
        let mut ir = IntermediateRepresentation::new(&self.project.name);

        // Find the task node
        let Some(task_node) = self.find_task_node(task_name) else {
            return Err(CompilerError::TaskNotFound(task_name.to_string()));
        };

        // Compile just this task node
        self.compile_task_node(task_name, task_node, &mut ir)?;

        Ok(ir)
    }

    /// Find a task node by name (handles dotted paths for nested tasks)
    /// Returns the TaskNode which can be a Task, Group, or List
    pub(super) fn find_task_node(&self, name: &str) -> Option<&TaskNode> {
        let parts: Vec<&str> = name.split('.').collect();
        let mut current_tasks = &self.project.tasks;

        for (i, part) in parts.iter().enumerate() {
            match current_tasks.get(*part) {
                Some(node) if i == parts.len() - 1 => {
                    return Some(node);
                }
                Some(TaskNode::Group(group)) => {
                    current_tasks = &group.children;
                }
                _ => return None,
            }
        }
        None
    }

    /// Find a leaf task by name (handles dotted paths for nested tasks)
    pub(super) fn find_task(&self, name: &str) -> Option<&Task> {
        match self.find_task_node(name) {
            Some(TaskNode::Task(task)) => Some(task),
            _ => None,
        }
    }

    /// Expand a dependency name to its leaf task names.
    ///
    /// If the dependency refers to:
    /// - A leaf Task: returns `[dep_name]`
    /// - A TaskGroup: returns all leaf children (recursively), sorted alphabetically
    /// - A Sequence: returns all leaf tasks in the sequence (recursively)
    /// - Non-existent: tries sibling resolution, then returns as-is for validation
    ///
    /// # Arguments
    /// * `dep_name` - The dependency name (may be simple like "build" or qualified like "docs.build")
    /// * `current_task_id` - The ID of the task that has this dependency (e.g., "docs.deploy")
    pub(super) fn expand_dependency_to_leaf_tasks(
        &self,
        dep_name: &str,
        current_task_id: &str,
    ) -> Vec<String> {
        // First, try direct lookup
        if let Some(node) = self.find_task_node(dep_name) {
            let mut result = Vec::new();
            Self::collect_leaf_task_names(dep_name, node, &mut result);
            result.sort();
            return result;
        }

        // Not found directly - try sibling resolution
        // If current task is "docs.deploy" and dep is "build", try "docs.build"
        if let Some(parent_path) = current_task_id.rsplit_once('.').map(|(parent, _)| parent) {
            let sibling_path = format!("{parent_path}.{dep_name}");
            if let Some(node) = self.find_task_node(&sibling_path) {
                let mut result = Vec::new();
                Self::collect_leaf_task_names(&sibling_path, node, &mut result);
                result.sort();
                return result;
            }
        }

        // Task not found - return as-is and let validation report the error
        vec![dep_name.to_string()]
    }

    /// Recursively collect all leaf task names from a TaskNode.
    /// This is a helper function, not a method, since it doesn't need compiler state.
    pub(super) fn collect_leaf_task_names(prefix: &str, node: &TaskNode, result: &mut Vec<String>) {
        match node {
            TaskNode::Task(_) => {
                result.push(prefix.to_string());
            }
            TaskNode::Group(group) => {
                for (child_name, child_node) in &group.children {
                    Self::collect_leaf_task_names(
                        &format!("{prefix}.{child_name}"),
                        child_node,
                        result,
                    );
                }
            }
            TaskNode::Sequence(steps) => {
                for (idx, child_node) in steps.iter().enumerate() {
                    Self::collect_leaf_task_names(&format!("{prefix}.{idx}"), child_node, result);
                }
            }
        }
    }

    /// Compile task nodes into IR tasks
    pub(super) fn compile_tasks(
        &self,
        tasks: &HashMap<String, TaskNode>,
        ir: &mut IntermediateRepresentation,
    ) -> Result<(), CompilerError> {
        // Sort keys for deterministic output
        let mut sorted_keys: Vec<_> = tasks.keys().collect();
        sorted_keys.sort();
        for name in sorted_keys {
            let task_node = &tasks[name];
            self.compile_task_node(name, task_node, ir)?;
        }
        Ok(())
    }

    /// Compile a task node (handles tasks, groups, and lists)
    pub(super) fn compile_task_node(
        &self,
        name: &str,
        node: &TaskNode,
        ir: &mut IntermediateRepresentation,
    ) -> Result<(), CompilerError> {
        match node {
            TaskNode::Task(task) => {
                let ir_task = self.compile_single_task(name, task)?;
                ir.tasks.push(ir_task);
            }
            TaskNode::Group(group) => {
                self.compile_task_group(name, group, ir)?;
            }
            TaskNode::Sequence(steps) => {
                self.compile_task_sequence(name, steps, ir)?;
            }
        }
        Ok(())
    }

    /// Compile a task group (parallel execution)
    pub(super) fn compile_task_group(
        &self,
        prefix: &str,
        group: &TaskGroup,
        ir: &mut IntermediateRepresentation,
    ) -> Result<(), CompilerError> {
        // Sort keys for deterministic output
        let mut sorted_keys: Vec<_> = group.children.keys().collect();
        sorted_keys.sort();
        for name in sorted_keys {
            let child_node = &group.children[name];
            let task_name = format!("{prefix}.{name}");
            self.compile_task_node(&task_name, child_node, ir)?;
        }
        Ok(())
    }

    /// Compile a task sequence (sequential execution)
    pub(super) fn compile_task_sequence(
        &self,
        prefix: &str,
        steps: &[TaskNode],
        ir: &mut IntermediateRepresentation,
    ) -> Result<(), CompilerError> {
        for (idx, child_node) in steps.iter().enumerate() {
            let task_name = format!("{prefix}.{idx}");
            self.compile_task_node(&task_name, child_node, ir)?;
        }
        Ok(())
    }

    /// Compile a single task to IR format
    pub(super) fn compile_single_task(
        &self,
        id: &str,
        task: &Task,
    ) -> Result<IrTask, CompilerError> {
        // Convert command and args to array format
        let command = if !task.command.is_empty() {
            let mut cmd = vec![task.command.clone()];
            cmd.extend(task.args.clone());
            cmd
        } else if task.script.is_some() {
            // The invocation `cuenv task` runs: the task's script shell (bash
            // unless configured) with its shellOptions applied.
            let spec = task.command_spec(str::to_string).map_err(|error| {
                CompilerError::InvalidTaskStructure(format!("Task '{id}': {error}"))
            })?;
            std::iter::once(spec.program).chain(spec.args).collect()
        } else {
            return Err(CompilerError::InvalidTaskStructure(format!(
                "Task '{id}' has neither command nor script"
            )));
        };

        // A script's invocation already names its shell.
        let shell = task.shell.is_some() && task.script.is_none();

        // Convert environment variables (filter out complex JSON values)
        let env: BTreeMap<String, String> = task
            .env
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect();

        // Extract secrets (simplified - would integrate with secret resolver)
        let secrets: BTreeMap<String, SecretConfig> = BTreeMap::new();

        // Convert inputs (path globs only for now)
        let inputs: Vec<String> = task.iter_path_inputs().cloned().collect();

        // Convert outputs - use Orchestrator in CI mode for cross-job artifact sharing
        let output_type = if self.options.ci_mode {
            OutputType::Orchestrator
        } else {
            OutputType::Cas
        };
        let outputs: Vec<OutputDeclaration> = task
            .outputs
            .iter()
            .map(|path| OutputDeclaration {
                path: path.clone(),
                output_type,
            })
            .collect();

        // Convert task output references to artifact downloads (CI mode only)
        let artifact_downloads: Vec<ArtifactDownload> = if self.options.ci_mode {
            task.iter_task_output_names()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(|task_name| {
                    // Use the task name to construct artifact name
                    // The path should match where the artifact was uploaded from
                    ArtifactDownload {
                        name: format!("{}-artifacts", task_name.replace('.', "-")),
                        path: task_name.replace('.', "/"),
                        filter: String::new(),
                    }
                })
                .collect()
        } else {
            vec![]
        };

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
            depends_on: task
                .depends_on
                .iter()
                .flat_map(|d| self.expand_dependency_to_leaf_tasks(d.task_name(), id))
                .collect(),
            cache_policy,
            deployment,
            manual_approval: false, // Would come from task metadata
            matrix: None,
            artifact_downloads,
            params: BTreeMap::new(),
            // Phase task fields (not applicable for regular tasks)
            phase: None,
            label: None,
            priority: None,
            contributor: None,
            condition: None,
            github_task_filter: Vec::new(),
            github_permissions: task
                .ci
                .as_ref()
                .and_then(|ci| ci.github.as_ref())
                .map(|gh| gh.permissions.clone())
                .unwrap_or_default(),
            provider_hints: None,
        })
    }

    /// Fix artifact download paths to use actual upstream task output paths.
    ///
    /// During initial compilation, artifact_downloads are created with paths derived
    /// from task names (e.g., "docs.build" → "docs/build"). This post-processing step
    /// updates those paths to use the actual output paths from the upstream tasks
    /// (e.g., "docs/dist" if that's what docs.build outputs).
    pub(super) fn fix_artifact_download_paths(ir: &mut IntermediateRepresentation) {
        // Build a lookup map: task_id → first output path
        // We use the first output path as the download destination
        let task_outputs: HashMap<String, String> = ir
            .tasks
            .iter()
            .filter_map(|task| {
                task.outputs
                    .first()
                    .map(|output| (task.id.clone(), output.path.clone()))
            })
            .collect();

        // Update artifact downloads to use actual upstream output paths
        for task in &mut ir.tasks {
            for download in &mut task.artifact_downloads {
                // Extract task ID from artifact name (e.g., "docs-build-artifacts" → "docs.build")
                let upstream_task_id = download
                    .name
                    .strip_suffix("-artifacts")
                    .map(|s| s.replace('-', "."))
                    .unwrap_or_default();

                // If we have the upstream task's output path, use it
                if let Some(output_path) = task_outputs.get(&upstream_task_id) {
                    download.path.clone_from(output_path);
                }
            }
        }
    }
}
