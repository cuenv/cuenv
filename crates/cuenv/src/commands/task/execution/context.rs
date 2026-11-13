//! Context for task selection and execution.

use super::super::types::{ExecutionMode, OutputConfig, TaskExecutionRequest, TaskSelection};
use cuenv_core::manifest::Project;
use cuenv_core::tasks::Tasks;
use cuenv_core::{DryRun, OutputCapture};
use cuenv_task_exec::TaskIndex;
use std::path::PathBuf;

pub(super) struct TaskExecutionInput<'input> {
    pub(super) selection: &'input TaskSelection,
    pub(super) output: &'input OutputConfig,
    pub(super) execution_mode: &'input ExecutionMode,
    pub(super) path: &'input str,
    pub(super) package: &'input str,
    pub(super) environment: Option<&'input str>,
    pub(super) format: &'input str,
    pub(super) capture_output: OutputCapture,
    pub(super) skip_dependencies: bool,
    pub(super) continue_on_error: bool,
    pub(super) dry_run: DryRun,
    pub(super) executor: &'input crate::commands::CommandExecutor,
    pub(super) task_name: Option<&'input str>,
    pub(super) labels: &'input [String],
    pub(super) task_args: &'input [String],
}

impl<'input> TaskExecutionInput<'input> {
    pub(super) fn from_request(request: &'input TaskExecutionRequest<'_>) -> Self {
        let (task_name, labels, task_args) = match &request.selection {
            TaskSelection::Named { name, args } => (Some(name.as_str()), &[][..], args.as_slice()),
            TaskSelection::Labels(labels) => (None, labels.as_slice(), &[][..]),
            TaskSelection::List | TaskSelection::Interactive => (None, &[][..], &[][..]),
        };

        Self {
            selection: &request.selection,
            output: &request.output,
            execution_mode: &request.execution_mode,
            path: &request.path,
            package: &request.package,
            environment: request.environment.as_deref(),
            format: &request.output.format,
            capture_output: request.output.capture_output,
            skip_dependencies: request.skip_dependencies,
            continue_on_error: request.continue_on_error,
            dry_run: request.dry_run,
            executor: request.executor,
            task_name,
            labels,
            task_args,
        }
    }

    pub(super) fn should_show_general_help(&self) -> bool {
        self.task_name.is_none() && self.output.help
    }

    pub(super) fn uses_interactive_picker(&self) -> bool {
        matches!(self.selection, TaskSelection::Interactive)
            && self.task_name.is_none()
            && self.labels.is_empty()
    }

    pub(super) fn lists_tasks(&self) -> bool {
        self.task_name.is_none() && self.labels.is_empty()
    }

    pub(super) fn shows_help(&self) -> bool {
        self.output.help
    }

    pub(super) fn uses_tui(&self) -> bool {
        self.execution_mode == &ExecutionMode::Tui
    }
}

pub(super) struct TaskExecutionContext {
    pub(super) manifest: Project,
    pub(super) project_root: PathBuf,
    pub(super) cue_module_root: Option<PathBuf>,
    pub(super) task_index: TaskIndex,
    /// The project's own tasks by canonical name. A run selects from these,
    /// by name or by label, and from nothing else: the tasks of another
    /// project are reached as dependencies only.
    pub(super) local_tasks: Tasks,
}
