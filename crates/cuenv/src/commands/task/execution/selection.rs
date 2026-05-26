use super::{TaskExecutionContext, TaskExecutionInput};
use crate::commands::relative_path_from_root;
use crate::commands::task::arguments::{apply_args_to_task, resolve_task_args};
use crate::commands::task::discovery::{
    find_tasks_with_labels, format_label_root, normalize_labels,
};
use cuenv_core::Result;
use cuenv_core::tasks::{Task, TaskNode, Tasks};
use std::path::Path;

/// Resolved task context from either named-task or label-based resolution.
pub(super) struct TaskResolution {
    pub(super) display_name: String,
    pub(super) node: TaskNode,
    pub(super) tasks: Tasks,
    pub(super) graph_root_name: String,
    pub(super) output_ref_deps: Vec<(String, String)>,
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

    let normalized_labels = normalize_labels(input.labels);
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
    let selected_task_node = selected_task_node(input, &mut tasks_in_scope, &display_task_name)?;

    Ok(TaskResolution {
        display_name: display_task_name.clone(),
        node: selected_task_node,
        tasks: tasks_in_scope,
        graph_root_name: display_task_name,
        output_ref_deps: current_instance_output_ref_deps(input.executor, &context.project_root)?,
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

fn selected_task_node(
    input: &TaskExecutionInput<'_>,
    tasks_in_scope: &mut Tasks,
    display_task_name: &str,
) -> Result<TaskNode> {
    let node = tasks_in_scope
        .get(display_task_name)
        .cloned()
        .ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "Task '{display_task_name}' not found in local tasks"
            ))
        })?;

    if input.task_args.is_empty() {
        return Ok(node);
    }

    let TaskNode::Task(task) = node else {
        return Err(cuenv_core::Error::configuration(
            "Task arguments are not supported for task groups or lists".to_string(),
        ));
    };

    let resolved_args = resolve_task_args(task.params.as_ref(), input.task_args)?;
    tracing::debug!("Resolved task args: {:?}", resolved_args);

    let modified_task = apply_args_to_task(&task, &resolved_args);
    let modified_node = TaskNode::Task(Box::new(modified_task.clone()));
    tasks_in_scope
        .tasks
        .insert(display_task_name.to_string(), modified_node.clone());
    Ok(modified_node)
}

fn resolve_label_tasks(
    input: &TaskExecutionInput<'_>,
    context: &TaskExecutionContext,
    normalized_labels: &[String],
) -> Result<TaskResolution> {
    let mut tasks_in_scope = context.local_tasks.clone();
    let matching_tasks = find_tasks_with_labels(&context.local_tasks, normalized_labels);

    if matching_tasks.is_empty() {
        return Err(cuenv_core::Error::configuration(format!(
            "No tasks with labels {normalized_labels:?} were found in this scope"
        )));
    }

    let display_task_name = format_label_root(normalized_labels);
    let synthetic = Task {
        script: Some("true".to_string()),
        hermetic: false,
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

    let resolved_node = tasks_in_scope
        .get(&display_task_name)
        .cloned()
        .ok_or_else(|| cuenv_core::Error::execution("synthetic task missing after insertion"))?;

    Ok(TaskResolution {
        display_name: display_task_name.clone(),
        node: resolved_node,
        tasks: tasks_in_scope,
        graph_root_name: display_task_name,
        output_ref_deps: current_instance_output_ref_deps(input.executor, &context.project_root)?,
    })
}

fn current_instance_output_ref_deps(
    executor: &crate::commands::CommandExecutor,
    project_root: &Path,
) -> Result<Vec<(String, String)>> {
    let module = executor.get_module(project_root)?;
    let rel_path = relative_path_from_root(&module.root, project_root);

    Ok(module
        .get(&rel_path)
        .map_or_else(Vec::new, |instance| instance.output_ref_deps.clone()))
}
