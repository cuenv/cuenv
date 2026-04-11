//! Implementation of the `cuenv ps` command.
//!
//! Lists running services and their status from session state.

use std::path::Path;

use cuenv_events::emit_stdout;
use cuenv_services::session::SessionManager;

/// Options for the `cuenv ps` command.
pub struct PsOptions {
    /// Path to directory containing CUE files.
    pub path: String,
    /// CUE package name to evaluate.
    pub package: String,
    /// Output format (table or json).
    pub output_format: String,
}

/// Execute the `cuenv ps` command.
///
/// # Errors
///
/// Returns an error if no session exists or state files can't be read.
pub fn execute_ps(options: &PsOptions) -> cuenv_core::Result<String> {
    let project_path = Path::new(&options.path);
    let session = SessionManager::load(project_path)
        .map_err(|e| cuenv_core::Error::execution(format!("Failed to load session: {e}")))?;

    let services = session
        .list_services()
        .map_err(|e| cuenv_core::Error::execution(format!("Failed to list services: {e}")))?;

    if services.is_empty() {
        emit_stdout!("No services found.");
        return Ok(String::new());
    }

    if options.output_format == "json" {
        let json = serde_json::to_string_pretty(&services)
            .map_err(|e| cuenv_core::Error::execution(format!("Failed to serialize: {e}")))?;
        emit_stdout!(json);
    } else {
        // Table format
        emit_stdout!(format!(
            "{:<20} {:<12} {:<12} {:<10} {:<8}",
            "NAME", "STATE", "UPTIME", "RESTARTS", "PID"
        ));
        emit_stdout!(format!("{}", "-".repeat(62)));

        for svc in &services {
            let uptime = match svc.started_at {
                Some(t) => {
                    let elapsed = chrono::Utc::now().signed_duration_since(t);
                    format_duration(elapsed)
                }
                None => "-".to_string(),
            };

            let pid = match svc.pid {
                Some(p) => p.to_string(),
                None => "-".to_string(),
            };

            emit_stdout!(format!(
                "{:<20} {:<12} {:<12} {:<10} {:<8}",
                svc.name, svc.lifecycle, uptime, svc.restarts, pid
            ));
        }
    }

    Ok(String::new())
}

/// Format a chrono duration as a human-readable string.
fn format_duration(duration: chrono::TimeDelta) -> String {
    let secs = duration.num_seconds();
    if secs < 0 {
        return "-".to_string();
    }

    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;

    if hours > 0 {
        format!("{hours}h{minutes}m{seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m{seconds}s")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration() {
        assert_eq!(
            format_duration(chrono::TimeDelta::try_seconds(90).unwrap()),
            "1m30s"
        );
        assert_eq!(
            format_duration(chrono::TimeDelta::try_seconds(3661).unwrap()),
            "1h1m1s"
        );
        assert_eq!(
            format_duration(chrono::TimeDelta::try_seconds(5).unwrap()),
            "5s"
        );
    }
}
