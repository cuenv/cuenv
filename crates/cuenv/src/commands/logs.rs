//! Implementation of the `cuenv logs` command.
//!
//! Tails persisted service log files from the session state directory.

use std::path::Path;

use cuenv_events::emit_stdout;
use cuenv_services::session::SessionManager;

/// Options for the `cuenv logs` command.
pub struct LogsOptions {
    /// Path to directory containing CUE files.
    pub path: String,
    /// CUE package name to evaluate.
    pub package: String,
    /// Service names to view logs for (empty = all).
    pub services: Vec<String>,
    /// Follow log output.
    pub follow: bool,
    /// Number of lines to show.
    pub lines: usize,
}

/// Execute the `cuenv logs` command.
///
/// # Errors
///
/// Returns an error if no session exists or log files can't be read.
pub fn execute_logs(options: &LogsOptions) -> cuenv_core::Result<String> {
    let project_path = Path::new(&options.path);
    let session = SessionManager::load(project_path).map_err(|e| {
        cuenv_core::Error::execution(format!("Failed to load session: {e}"))
    })?;

    let services = if options.services.is_empty() {
        session
            .list_services()
            .map_err(|e| cuenv_core::Error::execution(format!("Failed to list services: {e}")))?
            .into_iter()
            .map(|s| s.name)
            .collect()
    } else {
        options.services.clone()
    };

    for name in &services {
        let log_path = session.log_path(name);
        if !log_path.exists() {
            emit_stdout!(format!("[{name}] (no logs)"));
            continue;
        }

        let content = std::fs::read_to_string(&log_path).map_err(|e| {
            cuenv_core::Error::execution(format!("Failed to read log for {name}: {e}"))
        })?;

        let all_lines: Vec<&str> = content.lines().collect();
        let start = all_lines.len().saturating_sub(options.lines);
        for line in &all_lines[start..] {
            emit_stdout!(format!("[{name}] {line}"));
        }
    }

    if options.follow {
        emit_stdout!("Follow mode (--follow) requires a running session. Use Ctrl-C to stop.");
        // TODO: Implement follow mode with file watching in a future iteration.
        // For now, we just show existing logs.
    }

    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logs_options() {
        let options = LogsOptions {
            path: ".".to_string(),
            package: "cuenv".to_string(),
            services: vec![],
            follow: false,
            lines: 100,
        };
        assert_eq!(options.lines, 100);
        assert!(!options.follow);
    }
}
