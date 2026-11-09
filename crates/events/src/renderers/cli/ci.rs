use super::{CliRenderer, cli_renderer};
use crate::event::CiEvent;

impl CliRenderer {
    pub(super) fn render_ci(event: &CiEvent) {
        match event {
            CiEvent::ContextDetected {
                provider,
                event_type,
                ref_name,
            } => {
                cli_renderer::stdout_line(format_args!(
                    "Context: {provider} (event: {event_type}, ref: {ref_name})"
                ));
            }
            CiEvent::ChangedFilesFound { count } => {
                cli_renderer::stdout_line(format_args!("Changed files: {count}"));
            }
            CiEvent::ProjectsDiscovered { count } => {
                cli_renderer::stdout_line(format_args!("Found {count} projects"));
            }
            CiEvent::ProjectSkipped { path, reason } => {
                cli_renderer::stdout_line(format_args!("Project {path}: {reason}"));
            }
            CiEvent::TaskExecuting { task, .. } => {
                cli_renderer::stdout_line(format_args!("  -> Executing {task}"));
            }
            CiEvent::TaskResult {
                task,
                success,
                error,
                ..
            } => {
                if *success {
                    cli_renderer::stdout_line(format_args!("  -> {task} passed"));
                } else if let Some(err) = error {
                    cli_renderer::stdout_line(format_args!("  -> {task} failed: {err}"));
                } else {
                    cli_renderer::stdout_line(format_args!("  -> {task} failed"));
                }
            }
            CiEvent::ReportGenerated { path } => {
                cli_renderer::stdout_line(format_args!("Report written to: {path}"));
            }
        }
    }
}
