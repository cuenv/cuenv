use async_trait::async_trait;
use cuenv_core::Result;

use crate::commands::{Command, CommandExecutor, task};

use super::CommandHandler;

/// Handler for `task` command.
pub struct TaskHandler<'a> {
    request: task::TaskExecutionRequest<'a>,
}

impl<'a> TaskHandler<'a> {
    /// Build a task handler from the parsed task command.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when task selection options conflict.
    pub fn from_command(command: Command, executor: &'a CommandExecutor) -> Result<Self> {
        let Command::Task {
            path,
            package,
            name,
            labels,
            environment,
            format,
            materialize_outputs,
            show_cache_path,
            backend,
            tui,
            interactive,
            help,
            skip_dependencies,
            continue_on_error,
            dry_run,
            task_args,
        } = command
        else {
            unreachable!("task handler requires task command");
        };

        if !labels.is_empty() && name.is_some() {
            return Err(cuenv_core::Error::configuration(
                "Cannot specify both a task name and --label",
            ));
        }
        if !labels.is_empty() && !task_args.is_empty() {
            return Err(cuenv_core::Error::configuration(
                "Task arguments are not supported when selecting tasks by label",
            ));
        }

        let mut request = match (name, labels, interactive) {
            (Some(name), _, _) => task::TaskExecutionRequest::named(path, package, name, executor)
                .with_args(task_args),
            (None, labels, _) if !labels.is_empty() => {
                task::TaskExecutionRequest::labels(path, package, labels, executor)
            }
            (None, _, true) => task::TaskExecutionRequest::interactive(path, package, executor),
            (None, _, _) => task::TaskExecutionRequest::list(path, package, executor),
        };

        if let Some(env) = environment {
            request = request.with_environment(env);
        }
        request = request.with_format(format);
        if let Some(path) = materialize_outputs {
            request = request.with_materialize_outputs(path);
        }
        if show_cache_path {
            request = request.with_show_cache_path();
        }
        if let Some(backend) = backend {
            request = request.with_backend(backend);
        }
        if tui {
            request = request.with_tui();
        }
        if help {
            request = request.with_help();
        }
        if skip_dependencies {
            request = request.with_skip_dependencies();
        }
        if continue_on_error {
            request = request.with_continue_on_error();
        }
        if dry_run.is_dry_run() {
            request = request.with_dry_run();
        }

        Ok(Self { request })
    }
}

#[async_trait]
impl CommandHandler for TaskHandler<'_> {
    fn command_name(&self) -> &'static str {
        "task"
    }

    async fn execute(&self, _executor: &CommandExecutor) -> Result<String> {
        task::execute(self.request.clone()).await
    }
}
