use super::CliRenderer;
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
                println!("Context: {provider} (event: {event_type}, ref: {ref_name})");
            }
            CiEvent::ChangedFilesFound { count } => {
                println!("Changed files: {count}");
            }
            CiEvent::ProjectsDiscovered { count } => {
                println!("Found {count} projects");
            }
            CiEvent::ProjectSkipped { path, reason } => {
                println!("Project {path}: {reason}");
            }
            CiEvent::TaskExecuting { task, .. } => {
                println!("  -> Executing {task}");
            }
            CiEvent::TaskResult {
                task,
                success,
                error,
                ..
            } => {
                if *success {
                    println!("  -> {task} passed");
                } else if let Some(err) = error {
                    println!("  -> {task} failed: {err}");
                } else {
                    println!("  -> {task} failed");
                }
            }
            CiEvent::ReportGenerated { path } => {
                println!("Report written to: {path}");
            }
        }
    }
}
