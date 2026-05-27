use super::{AutoAssociate, CONTRIBUTOR_TASK_PREFIX, Contributor, ContributorContext};
use crate::Result;
use crate::tasks::{Input, Task, TaskDependency, TaskNode};
use std::collections::HashMap;

/// Engine that applies contributors to modify the task DAG
pub struct ContributorEngine<'a> {
    contributors: &'a [Contributor],
    context: ContributorContext,
}

impl<'a> ContributorEngine<'a> {
    /// Create a new contributor engine
    #[must_use]
    pub fn new(contributors: &'a [Contributor], context: ContributorContext) -> Self {
        Self {
            contributors,
            context,
        }
    }

    /// Apply all active contributors to the task DAG
    ///
    /// Loops until no contributor makes changes (stable DAG).
    /// Returns the number of tasks injected.
    pub fn apply(&self, tasks: &mut HashMap<String, TaskNode>) -> Result<usize> {
        let mut total_injected = 0;
        let max_iterations = 10;

        for iteration in 0..max_iterations {
            let mut changed = false;

            for contributor in self.contributors {
                if self.is_active(contributor) {
                    let injected = self.inject_tasks(contributor, tasks);
                    if injected > 0 {
                        changed = true;
                        total_injected += injected;
                        tracing::debug!(
                            contributor = %contributor.id,
                            injected,
                            "Contributor injected tasks"
                        );
                    }

                    if let Some(auto_assoc) = &contributor.auto_associate {
                        self.apply_auto_association(auto_assoc, tasks);
                    }
                }
            }

            if !changed {
                tracing::debug!(
                    iterations = iteration + 1,
                    total_injected,
                    "Contributor loop stabilized"
                );
                break;
            }
        }

        Ok(total_injected)
    }

    /// Check if a contributor should be active based on its conditions
    pub(crate) fn is_active(&self, contributor: &Contributor) -> bool {
        let Some(when) = &contributor.when else {
            return true;
        };

        if when.always == Some(true) {
            return true;
        }

        if !when.workspace_member.is_empty() {
            let has_match = self.context.workspace_member.as_ref().is_some_and(|ws| {
                when.workspace_member
                    .iter()
                    .any(|w| w.eq_ignore_ascii_case(ws))
            });
            if !has_match {
                return false;
            }
        }

        if !when.command.is_empty() {
            let has_match = when
                .command
                .iter()
                .any(|cmd| self.context.task_commands.contains(cmd));
            if !has_match {
                return false;
            }
        }

        if !when.service_command.is_empty() {
            let has_match = when
                .service_command
                .iter()
                .any(|cmd| self.context.service_commands.contains(cmd));
            if !has_match {
                return false;
            }
        }

        if when.has_service == Some(true) && !self.context.has_services {
            return false;
        }
        if when.has_service == Some(false) && self.context.has_services {
            return false;
        }

        true
    }

    /// Inject tasks from a contributor into the DAG
    ///
    /// Returns the number of tasks injected
    fn inject_tasks(
        &self,
        contributor: &Contributor,
        tasks: &mut HashMap<String, TaskNode>,
    ) -> usize {
        let mut injected = 0;

        for contrib_task in &contributor.tasks {
            let task_id = if contrib_task.id.starts_with(CONTRIBUTOR_TASK_PREFIX) {
                contrib_task.id.clone()
            } else {
                format!("{}{}", CONTRIBUTOR_TASK_PREFIX, contrib_task.id)
            };

            if tasks.contains_key(&task_id) {
                continue;
            }

            let task = Task {
                command: contrib_task.command.clone().unwrap_or_default(),
                args: contrib_task.args.clone(),
                script: contrib_task.script.clone(),
                inputs: contrib_task
                    .inputs
                    .iter()
                    .map(|s| Input::Path(s.clone()))
                    .collect(),
                outputs: contrib_task.outputs.clone(),
                hermetic: contrib_task.hermetic,
                depends_on: contrib_task
                    .depends_on
                    .iter()
                    .map(|dep| {
                        let name =
                            if dep.starts_with(CONTRIBUTOR_TASK_PREFIX) || dep.starts_with('#') {
                                dep.clone()
                            } else {
                                format!("{}{}", CONTRIBUTOR_TASK_PREFIX, dep)
                            };
                        TaskDependency::from_name(name)
                    })
                    .collect(),
                description: contrib_task.description.clone(),
                ..Default::default()
            };

            tasks.insert(task_id.clone(), TaskNode::Task(Box::new(task)));
            injected += 1;

            tracing::trace!(task = %task_id, "Injected contributor task");
        }

        injected
    }

    /// Apply auto-association rules to existing tasks
    fn apply_auto_association(
        &self,
        auto_assoc: &AutoAssociate,
        tasks: &mut HashMap<String, TaskNode>,
    ) {
        let Some(inject_dep) = &auto_assoc.inject_dependency else {
            return;
        };

        if !tasks.contains_key(inject_dep) {
            return;
        }

        let task_names: Vec<String> = tasks.keys().cloned().collect();

        for task_name in task_names {
            if task_name.starts_with(CONTRIBUTOR_TASK_PREFIX) {
                continue;
            }

            let Some(node) = tasks.get_mut(&task_name) else {
                continue;
            };

            Self::auto_associate_node(node, &auto_assoc.command, inject_dep);
        }
    }

    /// Recursively apply auto-association to a task node
    fn auto_associate_node(node: &mut TaskNode, commands: &[String], inject_dep: &str) {
        match node {
            TaskNode::Task(task) => {
                let Some(base_cmd) = cuenv_workspaces::command_name(&task.command) else {
                    return;
                };

                if commands.iter().any(|c| c == &base_cmd)
                    && !task.depends_on.iter().any(|d| d.task_name() == inject_dep)
                {
                    task.depends_on.push(TaskDependency::from_name(inject_dep));
                    tracing::trace!(
                        command = %task.command,
                        dependency = %inject_dep,
                        "Auto-associated task with contributor"
                    );
                }
            }
            TaskNode::Group(group) => {
                for sub in group.children.values_mut() {
                    Self::auto_associate_node(sub, commands, inject_dep);
                }
            }
            TaskNode::Sequence(steps) => {
                for sub in steps {
                    Self::auto_associate_node(sub, commands, inject_dep);
                }
            }
        }
    }
}
