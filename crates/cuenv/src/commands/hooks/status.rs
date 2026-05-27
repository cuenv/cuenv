use crate::cli::StatusFormat;
use cuenv_hooks::{ExecutionStatus, HookExecutionState};

/// Format status based on requested format
pub(super) fn format_status(state: &HookExecutionState, format: StatusFormat) -> String {
    match format {
        StatusFormat::Text => state.progress_display(),
        StatusFormat::Short => match state.status {
            ExecutionStatus::Running => {
                format!("[{}/{}]", state.completed_hooks, state.total_hooks)
            }
            ExecutionStatus::Completed => "[OK]".to_string(),
            ExecutionStatus::Failed => "[ERR]".to_string(),
            ExecutionStatus::Cancelled => "[X]".to_string(),
        },
        StatusFormat::Starship => format_starship_status(state),
    }
}

/// Format status for starship integration with rich information
fn format_starship_status(state: &HookExecutionState) -> String {
    match state.status {
        ExecutionStatus::Running => {
            // Show current hook name + duration
            if let Some(hook_display) = state.current_hook_display() {
                if let Some(duration) = state.current_hook_duration() {
                    let duration_str = HookExecutionState::format_duration(duration);
                    format!("cuenv hook {hook_display} ({duration_str})")
                } else {
                    // Just started, no duration yet - use overall execution time
                    let duration = state.duration();
                    let duration_str = HookExecutionState::format_duration(duration);
                    format!("cuenv hook {hook_display} ({duration_str})")
                }
            } else {
                // Fallback if no current hook (shouldn't happen in Running state)
                format!("🔄 {}/{}", state.completed_hooks, state.total_hooks)
            }
        }
        ExecutionStatus::Completed => {
            // Only show if within display timeout (ensures at least one display)
            if state.should_display_completed() {
                let duration = state.duration();
                let duration_str = HookExecutionState::format_duration(duration);
                format!("✅ {duration_str}")
            } else {
                // State has expired, return empty string to hide from prompt
                String::new()
            }
        }
        ExecutionStatus::Failed => {
            // Show failed state with error if within display timeout
            if state.should_display_completed() {
                if let Some(error_msg) = &state.error_message {
                    // Extract just the command name from error if possible
                    format!("❌ {}", error_msg.lines().next().unwrap_or("failed"))
                } else {
                    "❌ failed".to_string()
                }
            } else {
                String::new()
            }
        }
        ExecutionStatus::Cancelled => {
            if state.should_display_completed() {
                "🚫 cancelled".to_string()
            } else {
                String::new()
            }
        }
    }
}
