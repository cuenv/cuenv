use super::{TaskExecutionContext, TaskExecutionInput};
use crate::commands::task_list::{
    DashboardFormatter, EmojiFormatter, RichFormatter, TablesFormatter, TaskListFormatter,
    TextFormatter, build_task_list,
};
use cuenv_core::Result;
use std::io::IsTerminal;

pub(super) fn maybe_render_task_list(
    input: &TaskExecutionInput<'_>,
    context: &TaskExecutionContext,
) -> Result<Option<String>> {
    if !input.lists_tasks() {
        return Ok(None);
    }

    tracing::debug!("Listing available tasks");
    let tasks = context.task_index.list();
    tracing::debug!("Found {} tasks to list", tasks.len());

    if input.format == "json" {
        return serde_json::to_string(&tasks).map(Some).map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to serialize tasks: {e}"))
        });
    }

    if tasks.is_empty() {
        return Ok(Some("No tasks defined in the configuration".to_string()));
    }

    let cwd_relative = context.cue_module_root.as_ref().and_then(|root| {
        context
            .project_root
            .strip_prefix(root)
            .ok()
            .map(|path| path.to_string_lossy().to_string())
    });
    let task_data = build_task_list(&tasks, cwd_relative.as_deref(), &context.project_root);
    let effective_format = if input.format.is_empty() {
        context
            .manifest
            .config
            .as_ref()
            .and_then(|config| config.task_list_format())
            .map(|format| format.as_str())
    } else {
        Some(input.format)
    };

    let output = match effective_format {
        Some("rich") => RichFormatter::new().format(&task_data),
        Some("text") => TextFormatter.format(&task_data),
        Some("tables") => TablesFormatter::new().format(&task_data),
        Some("dashboard") => DashboardFormatter::new().format(&task_data),
        Some("emoji") => EmojiFormatter.format(&task_data),
        _ if std::io::stdout().is_terminal() => RichFormatter::new().format(&task_data),
        _ => TextFormatter.format(&task_data),
    };

    Ok(Some(output))
}
