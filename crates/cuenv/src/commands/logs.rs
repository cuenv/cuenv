//! Implementation of the `cuenv logs` command.
//!
//! Reads persisted service log files from the session state directory.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use cuenv_events::emit_stdout;
use cuenv_services::session::SessionManager;

const FOLLOW_POLL_INTERVAL: Duration = Duration::from_millis(500);

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
    let session = SessionManager::load(project_path)
        .map_err(|e| cuenv_core::Error::execution(format!("Failed to load session: {e}")))?;

    let services = log_service_names(&session, &options.services)?;
    let mut cursors = emit_existing_logs(&session, &services, options.lines)?;

    if options.follow {
        follow_logs(&session, &mut cursors)?;
    }

    Ok(String::new())
}

struct LogCursor {
    name: String,
    path: PathBuf,
    offset: usize,
}

fn log_service_names(
    session: &SessionManager,
    requested: &[String],
) -> cuenv_core::Result<Vec<String>> {
    if !requested.is_empty() {
        return Ok(requested.to_vec());
    }

    session
        .list_services()
        .map_err(|e| cuenv_core::Error::execution(format!("Failed to list services: {e}")))
        .map(|services| services.into_iter().map(|s| s.name).collect())
}

fn emit_existing_logs(
    session: &SessionManager,
    services: &[String],
    line_count: usize,
) -> cuenv_core::Result<Vec<LogCursor>> {
    let mut cursors = Vec::with_capacity(services.len());

    for name in services {
        let path = session.log_path(name);
        let offset = if let Some(content) = read_log_file(&path, name)? {
            for line in tail_lines(&content, line_count) {
                emit_stdout!(format!("[{name}] {line}"));
            }
            content.len()
        } else {
            emit_stdout!(format!("[{name}] (no logs)"));
            0
        };

        cursors.push(LogCursor {
            name: name.clone(),
            path,
            offset,
        });
    }

    Ok(cursors)
}

fn follow_logs(session: &SessionManager, cursors: &mut [LogCursor]) -> cuenv_core::Result<()> {
    if !session.is_alive() {
        return Err(cuenv_core::Error::execution(
            "Cannot follow logs because the service session controller is not running",
        ));
    }

    while session.is_alive() {
        thread::sleep(FOLLOW_POLL_INTERVAL);
        emit_new_log_lines(cursors)?;
    }

    emit_new_log_lines(cursors)?;
    emit_stdout!("Service session ended; stopping log follow.");
    Ok(())
}

fn emit_new_log_lines(cursors: &mut [LogCursor]) -> cuenv_core::Result<()> {
    for cursor in cursors {
        let Some(content) = read_log_file(&cursor.path, &cursor.name)? else {
            continue;
        };

        let (unread, next_offset) = unread_content(&content, cursor.offset);
        for line in unread.lines() {
            emit_stdout!(format!("[{}] {line}", cursor.name));
        }
        cursor.offset = next_offset;
    }

    Ok(())
}

fn read_log_file(path: &Path, name: &str) -> cuenv_core::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(cuenv_core::Error::execution(format!(
            "Failed to read log for {name}: {error}"
        ))),
    }
}

fn tail_lines(content: &str, line_count: usize) -> Vec<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(line_count);
    lines[start..].to_vec()
}

fn unread_content(content: &str, offset: usize) -> (&str, usize) {
    (content.get(offset..).unwrap_or(content), content.len())
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

    #[test]
    fn test_tail_lines_returns_last_lines() {
        let lines = tail_lines("one\ntwo\nthree\n", 2);
        assert_eq!(lines, vec!["two", "three"]);
    }

    #[test]
    fn test_tail_lines_zero_returns_empty_tail() {
        let lines = tail_lines("one\ntwo\n", 0);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_unread_content_starts_at_offset() {
        let (unread, next_offset) = unread_content("one\ntwo\nthree\n", "one\n".len());
        assert_eq!(unread, "two\nthree\n");
        assert_eq!(next_offset, "one\ntwo\nthree\n".len());
    }

    #[test]
    fn test_unread_content_handles_truncated_file() {
        let (unread, next_offset) = unread_content("new\n", 100);
        assert_eq!(unread, "new\n");
        assert_eq!(next_offset, "new\n".len());
    }
}
