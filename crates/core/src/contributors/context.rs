use crate::tasks::TaskNode;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Context provided to contributors for activation condition evaluation
#[derive(Debug, Clone, Default)]
pub struct ContributorContext {
    /// Detected workspace membership (e.g., "bun", "npm", "cargo")
    pub workspace_member: Option<String>,

    /// Path to workspace root (if member of a workspace)
    pub workspace_root: Option<PathBuf>,

    /// All commands used by tasks in the project (for command-based activation)
    pub task_commands: HashSet<String>,

    /// Commands used by services in the project (for service command-based activation)
    pub service_commands: HashSet<String>,

    /// Whether the project has any services defined
    pub has_services: bool,
}

impl ContributorContext {
    /// Create context by detecting workspace from project root
    #[must_use]
    pub fn detect(project_root: &Path) -> Self {
        let mut ctx = Self::default();

        if let Ok(managers) = cuenv_workspaces::detect_package_managers(project_root)
            && let Some(first) = managers.first()
        {
            ctx.workspace_member = Some(workspace_name_for_manager(*first).to_string());
        }

        ctx
    }

    /// Add task commands from a project's tasks
    pub fn with_task_commands(mut self, tasks: &HashMap<String, TaskNode>) -> Self {
        for node in tasks.values() {
            collect_commands_from_node(node, &mut self.task_commands);
        }
        self
    }

    /// Add service commands from a project's services
    pub fn with_services(mut self, services: &HashMap<String, crate::manifest::Service>) -> Self {
        self.has_services = !services.is_empty();
        for service in services.values() {
            if let Some(cmd) = service.primary_command()
                && let Some(cmd_name) = cuenv_workspaces::command_name(cmd)
            {
                self.service_commands.insert(cmd_name);
            }
        }
        self
    }
}

fn workspace_name_for_manager(manager: cuenv_workspaces::PackageManager) -> &'static str {
    match manager {
        cuenv_workspaces::PackageManager::Npm => "npm",
        cuenv_workspaces::PackageManager::Bun => "bun",
        cuenv_workspaces::PackageManager::Pnpm => "pnpm",
        cuenv_workspaces::PackageManager::YarnClassic
        | cuenv_workspaces::PackageManager::YarnModern => "yarn",
        cuenv_workspaces::PackageManager::Cargo => "cargo",
        cuenv_workspaces::PackageManager::Deno => "deno",
    }
}

fn collect_commands_from_node(node: &TaskNode, commands: &mut HashSet<String>) {
    match node {
        TaskNode::Task(task) => {
            if !task.command.is_empty()
                && let Some(cmd) = cuenv_workspaces::command_name(&task.command)
            {
                commands.insert(cmd);
            }
        }
        TaskNode::Group(group) => {
            for sub in group.children.values() {
                collect_commands_from_node(sub, commands);
            }
        }
        TaskNode::Sequence(steps) => {
            for sub in steps {
                collect_commands_from_node(sub, commands);
            }
        }
    }
}
