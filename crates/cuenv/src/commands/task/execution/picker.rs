use super::{TaskExecutionContext, TaskExecutionInput};
use crate::commands::task::execute;
use crate::commands::task::types::TaskExecutionRequest;
use crate::commands::task_picker::{PickerResult, SelectableTask, run_picker};
use cuenv_core::Result;
use cuenv_core::tasks::TaskNode;

pub(super) async fn maybe_run_interactive_picker(
    input: &TaskExecutionInput<'_>,
    context: &TaskExecutionContext,
) -> Result<Option<String>> {
    if !input.uses_interactive_picker() {
        return Ok(None);
    }

    let selectable = context
        .task_index
        .list()
        .iter()
        .map(|task| SelectableTask {
            name: task.name.clone(),
            description: task_description(&task.node),
        })
        .collect();

    match run_picker(selectable) {
        Ok(PickerResult::Selected(selected_task)) => {
            let request = selected_task_request(input, &selected_task);
            Box::pin(execute(request)).await.map(Some)
        }
        Ok(PickerResult::Cancelled) => Ok(Some(String::new())),
        Err(e) => Err(cuenv_core::Error::configuration(format!(
            "Interactive picker failed: {e}"
        ))),
    }
}

fn task_description(node: &TaskNode) -> Option<String> {
    match node {
        TaskNode::Task(task) => task.description.clone(),
        TaskNode::Group(group) => group.description.clone(),
        TaskNode::Sequence(_) => None,
    }
}

fn selected_task_request<'a>(
    input: &TaskExecutionInput<'a>,
    selected_task: &str,
) -> TaskExecutionRequest<'a> {
    let mut request =
        TaskExecutionRequest::named(input.path, input.package, selected_task, input.executor)
            .with_format(input.format);

    if let Some(env) = input.environment {
        request = request.with_environment(env);
    }
    if input.capture_output.should_capture() {
        request = request.with_capture();
    }
    if let Some(path) = input.materialize_outputs {
        request = request.with_materialize_outputs(path);
    }
    if input.shows_cache_path() {
        request = request.with_show_cache_path();
    }
    if let Some(backend) = input.backend {
        request = request.with_backend(backend);
    }
    if input.uses_tui() {
        request = request.with_tui();
    }
    if input.shows_help() {
        request = request.with_help();
    }
    if input.skip_dependencies {
        request = request.with_skip_dependencies();
    }

    request
}
