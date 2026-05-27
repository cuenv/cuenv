use super::{CliRenderer, stdout_line};
use crate::event::CiEvent;

impl CliRenderer {
    pub(super) fn render_ci(&self, event: &CiEvent) {
        let _ = &self.config; // Reserved for future CI rendering options.
        match event {
            CiEvent::ContextDetected {
                provider,
                event_type,
                ref_name,
            } => {
                stdout_line(format_args!(
                    "Context: {provider} (event: {event_type}, ref: {ref_name})"
                ));
            }
            CiEvent::ChangedFilesFound { count } => {
                stdout_line(format_args!("Changed files: {count}"));
            }
            CiEvent::ProjectsDiscovered { count } => {
                stdout_line(format_args!("Found {count} projects"));
            }
            CiEvent::ProjectSkipped { path, reason } => {
                stdout_line(format_args!("Project {path}: {reason}"));
            }
            CiEvent::TaskExecuting { task, .. } => {
                stdout_line(format_args!("  -> Executing {task}"));
            }
            CiEvent::TaskResult {
                task,
                success,
                error,
                ..
            } => {
                if *success {
                    stdout_line(format_args!("  -> {task} passed"));
                } else if let Some(err) = error {
                    stdout_line(format_args!("  -> {task} failed: {err}"));
                } else {
                    stdout_line(format_args!("  -> {task} failed"));
                }
            }
            CiEvent::ReportGenerated { path } => {
                stdout_line(format_args!("Report written to: {path}"));
            }
        }
    }
}
