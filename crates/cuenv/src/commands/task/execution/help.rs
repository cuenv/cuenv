use super::{TaskExecutionContext, TaskExecutionInput};
use crate::commands::task::rendering::{format_task_detail, render_task_tree};
use cuenv_core::Result;

pub(super) fn maybe_render_task_help(
    input: &TaskExecutionInput<'_>,
    context: &TaskExecutionContext,
) -> Result<Option<String>> {
    if !input.shows_help() {
        return Ok(None);
    }

    let requested_task = input.task_name.ok_or_else(|| {
        cuenv_core::Error::configuration("task name required when no labels provided")
    })?;
    let prefix = format!("{requested_task}.");
    let subtasks: Vec<&cuenv_core::tasks::IndexedTask> = context
        .task_index
        .list()
        .iter()
        .filter(|task| task.name == requested_task || task.name.starts_with(&prefix))
        .copied()
        .collect();

    if subtasks.is_empty() {
        return Err(cuenv_core::Error::configuration(format!(
            "Task '{requested_task}' not found",
        )));
    }

    if subtasks.len() == 1 && subtasks[0].name == requested_task {
        return Ok(Some(format_task_detail(subtasks[0])));
    }

    Ok(Some(render_task_tree(subtasks, None)))
}
