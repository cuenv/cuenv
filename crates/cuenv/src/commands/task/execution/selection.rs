use super::context::{TaskExecutionContext, TaskExecutionInput};

use crate::commands::task::discovery;
use crate::commands::task::projects::{self, ModuleProjects, WorkspaceContributors};
use cuenv_core::Result;
use cuenv_core::tasks::{Task, TaskNode, Tasks};
use cuenv_task_exec::{RootProject, ScopeProject, TaskScope};
use std::sync::Arc;

/// Resolved task context from either named-task or label-based resolution.
pub(super) struct TaskResolution {
    pub(super) display_name: String,
    pub(super) node: TaskNode,
    /// The project's own tasks, and, for the graph only, the tasks of the
    /// projects the selected task depends on.
    pub(super) scope: TaskScope,
    pub(super) graph_root_name: String,
}

/// The scope of a run of the task called `root`: the project's `tasks`, and
/// every project the task's dependencies lead to.
///
/// A project that cannot be loaded is not an error here: the graph reports
/// it for the task that depends on it.
fn task_scope(
    input: &TaskExecutionInput<'_>,
    context: &TaskExecutionContext,
    selected: SelectedTasks<'_>,
) -> Result<TaskScope> {
    let SelectedTasks { tasks, root } = selected;
    let mut scope = TaskScope::new(RootProject {
        project: ScopeProject {
            root: context.project_root.clone(),
            module_root: context
                .cue_module_root
                .clone()
                .unwrap_or_else(|| context.project_root.clone()),
            manifest: Arc::new(context.manifest.clone()),
        },
        tasks,
        output_ref_deps: projects::instance_output_ref_deps(input.executor, &context.project_root)?,
    })
    // The store hashing leaves out, so a file reference cannot match
    // into it either.
    .excluding_cache_store(super::super::cache::resolve_cache_root(
        &context.project_root,
    ));
    scope.reach(
        [root],
        &ModuleProjects::new(input.executor, WorkspaceContributors::Inject),
    );
    Ok(scope)
}

/// The tasks a run can select, and the one it was asked for.
struct SelectedTasks<'selected> {
    tasks: Tasks,
    root: &'selected str,
}

/// The selected node as its scope holds it, with its references to other
/// projects resolved.
fn node_in_scope(scope: &TaskScope, name: &str) -> Result<TaskNode> {
    scope.local().get(name).cloned().ok_or_else(|| {
        cuenv_core::Error::execution(format!("Task '{name}' is missing from its own scope"))
    })
}

pub(super) fn validate_task_selection(input: &TaskExecutionInput<'_>) -> Result<Vec<String>> {
    if !input.labels.is_empty() && input.task_name.is_some() {
        return Err(cuenv_core::Error::configuration(
            "Cannot specify both a task name and --label",
        ));
    }
    if !input.labels.is_empty() && !input.task_args.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "Task arguments are not supported when selecting tasks by label",
        ));
    }

    let normalized_labels = discovery::normalize_labels(input.labels);
    if !input.labels.is_empty() && normalized_labels.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "Labels cannot be empty or whitespace-only",
        ));
    }

    Ok(normalized_labels)
}

pub(super) fn resolve_task_selection(
    input: &TaskExecutionInput<'_>,
    context: &TaskExecutionContext,
    normalized_labels: &[String],
) -> Result<TaskResolution> {
    if normalized_labels.is_empty() {
        resolve_named_task(input, context)
    } else {
        resolve_label_tasks(input, context, normalized_labels)
    }
}

fn resolve_named_task(
    input: &TaskExecutionInput<'_>,
    context: &TaskExecutionContext,
) -> Result<TaskResolution> {
    let requested_task = input.task_name.ok_or_else(|| {
        cuenv_core::Error::configuration("task name required when no labels provided")
    })?;
    tracing::debug!("Looking for specific task: {}", requested_task);

    let task_entry = context.task_index.resolve(requested_task)?;
    let display_task_name = task_entry.name.clone();
    log_task_resolution_context(context, requested_task);
    let original_task_node = context.local_tasks.get(&display_task_name).ok_or_else(|| {
        cuenv_core::Error::configuration(format!("Task '{display_task_name}' not found"))
    })?;
    tracing::debug!("Found task node: {:?}", original_task_node);

    let mut tasks_in_scope = context.local_tasks.clone();
    apply_task_arguments(input, &mut tasks_in_scope, &display_task_name)?;
    let scope = task_scope(
        input,
        context,
        SelectedTasks {
            tasks: tasks_in_scope,
            root: &display_task_name,
        },
    )?;

    Ok(TaskResolution {
        display_name: display_task_name.clone(),
        node: node_in_scope(&scope, &display_task_name)?,
        scope,
        graph_root_name: display_task_name,
    })
}

fn log_task_resolution_context(context: &TaskExecutionContext, requested_task: &str) {
    tracing::debug!(
        "Task index entries: {:?}",
        context
            .task_index
            .list()
            .iter()
            .map(|task| task.name.as_str())
            .collect::<Vec<_>>()
    );
    tracing::debug!(
        "Indexed tasks for execution: {:?}",
        context.local_tasks.list_tasks()
    );
    tracing::debug!(
        "Requested task '{}' present: {}",
        requested_task,
        context.local_tasks.get(requested_task).is_some()
    );
}

/// Apply the command line's arguments to the selected task, in place.
fn apply_task_arguments(
    input: &TaskExecutionInput<'_>,
    tasks_in_scope: &mut Tasks,
    display_task_name: &str,
) -> Result<()> {
    let node = tasks_in_scope
        .get(display_task_name)
        .cloned()
        .ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "Task '{display_task_name}' not found in local tasks"
            ))
        })?;

    if input.task_args.is_empty() {
        return Ok(());
    }

    let TaskNode::Task(task) = node else {
        return Err(cuenv_core::Error::configuration(
            "Task arguments are not supported for task groups or lists".to_string(),
        ));
    };

    let resolved_args =
        crate::commands::task::arguments::resolve_task_args(task.params.as_ref(), input.task_args)?;
    tracing::debug!("Resolved task args: {:?}", resolved_args);

    let modified_task = crate::commands::task::arguments::apply_args_to_task(&task, &resolved_args);
    tasks_in_scope.tasks.insert(
        display_task_name.to_string(),
        TaskNode::Task(Box::new(modified_task)),
    );
    Ok(())
}

fn resolve_label_tasks(
    input: &TaskExecutionInput<'_>,
    context: &TaskExecutionContext,
    normalized_labels: &[String],
) -> Result<TaskResolution> {
    let mut tasks_in_scope = context.local_tasks.clone();
    let matching_tasks = discovery::find_tasks_with_labels(&context.local_tasks, normalized_labels);

    if matching_tasks.is_empty() {
        return Err(cuenv_core::Error::configuration(format!(
            "No tasks with labels {normalized_labels:?} were found in this scope"
        )));
    }

    let display_task_name = discovery::format_label_root(normalized_labels);
    let synthetic = Task {
        script: Some("true".to_string()),
        hermetic: cuenv_core::tasks::Hermetic::Enabled(false),
        depends_on: matching_tasks
            .into_iter()
            .map(cuenv_core::tasks::TaskDependency::from_name)
            .collect(),
        project_root: Some(context.project_root.clone()),
        description: Some(format!(
            "Run all tasks matching labels: {}",
            normalized_labels.join(", ")
        )),
        ..Default::default()
    };

    tasks_in_scope.tasks.insert(
        display_task_name.clone(),
        TaskNode::Task(Box::new(synthetic)),
    );

    let scope = task_scope(
        input,
        context,
        SelectedTasks {
            tasks: tasks_in_scope,
            root: &display_task_name,
        },
    )?;

    Ok(TaskResolution {
        display_name: display_task_name.clone(),
        node: node_in_scope(&scope, &display_task_name)?,
        scope,
        graph_root_name: display_task_name,
    })
}
