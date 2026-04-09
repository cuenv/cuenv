//! Session state management for running services.
//!
//! Persists service state under `.cuenv/run/<project-hash>/` for
//! `cuenv ps`, `cuenv logs`, `cuenv down`, and `cuenv restart`.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::lifecycle::ServiceLifecycle;

/// Top-level session metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Schema version (for forward compat).
    pub version: u32,
    /// When the session started.
    pub started_at: DateTime<Utc>,
    /// PID of the `cuenv up` controller process.
    pub controller_pid: u32,
    /// Canonical project path.
    pub project_path: String,
    /// Project name from CUE.
    pub project_name: String,
}

/// Per-service state persisted to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceState {
    /// Service name.
    pub name: String,
    /// Current lifecycle state.
    pub lifecycle: ServiceLifecycle,
    /// Service process PID (if running).
    pub pid: Option<u32>,
    /// When the service was first started.
    pub started_at: Option<DateTime<Utc>>,
    /// When the service became ready.
    pub ready_at: Option<DateTime<Utc>>,
    /// Number of restarts in current session.
    pub restarts: u32,
    /// Last exit code.
    pub exit_code: Option<i32>,
    /// Last error message.
    pub error: Option<String>,
}

/// Manages session state on disk.
pub struct SessionManager {
    root: PathBuf,
}

impl SessionManager {
    /// Create a new session for a project, writing `session.json`.
    ///
    /// Fails if an existing session's controller PID is still alive.
    ///
    /// # Errors
    ///
    /// Returns an error if the session directory can't be created or a live session exists.
    pub fn create(project_path: &Path, project_name: &str) -> crate::Result<Self> {
        let root = session_dir(project_path);

        // Check for existing live session
        if root.join("session.json").exists() {
            if let Ok(existing) = Self::load_info(&root)
                && is_pid_alive(existing.controller_pid)
            {
                return Err(crate::Error::Session {
                    message: format!(
                        "another cuenv up session is running (PID {})",
                        existing.controller_pid
                    ),
                    help: Some("Run `cuenv down` first, or kill the existing process".into()),
                });
            }
            // Stale session — clean up
            let _ = fs::remove_dir_all(&root);
        }

        fs::create_dir_all(root.join("state"))?;
        fs::create_dir_all(root.join("logs"))?;

        let info = SessionInfo {
            version: 1,
            started_at: Utc::now(),
            controller_pid: std::process::id(),
            project_path: project_path.to_string_lossy().into_owned(),
            project_name: project_name.to_string(),
        };

        let json = serde_json::to_string_pretty(&info)
            .map_err(|e| crate::Error::session(format!("failed to serialize session: {e}")))?;
        fs::write(root.join("session.json"), json)?;

        Ok(Self { root })
    }

    /// Load an existing session.
    ///
    /// # Errors
    ///
    /// Returns an error if no session exists or the session file is corrupt.
    pub fn load(project_path: &Path) -> crate::Result<Self> {
        let root = session_dir(project_path);
        if !root.join("session.json").exists() {
            return Err(crate::Error::Session {
                message: "no active session found".to_string(),
                help: Some("Run `cuenv up` first to start services".into()),
            });
        }
        Ok(Self { root })
    }

    /// Read session info.
    ///
    /// # Errors
    ///
    /// Returns an error if the session file can't be read.
    pub fn info(&self) -> crate::Result<SessionInfo> {
        Self::load_info(&self.root)
    }

    /// Whether the session controller process is still alive.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.info()
            .map(|info| is_pid_alive(info.controller_pid))
            .unwrap_or(false)
    }

    /// Update a service's state.
    ///
    /// # Errors
    ///
    /// Returns an error if the state file can't be written.
    pub fn update_service(&self, state: &ServiceState) -> crate::Result<()> {
        let path = self.root.join("state").join(format!("{}.json", state.name));
        let json = serde_json::to_string_pretty(state)
            .map_err(|e| crate::Error::session(format!("serialize failed: {e}")))?;
        fs::write(path, json)?;
        Ok(())
    }

    /// Read a service's state.
    ///
    /// # Errors
    ///
    /// Returns an error if the state file can't be read or parsed.
    pub fn read_service(&self, name: &str) -> crate::Result<ServiceState> {
        let path = self.root.join("state").join(format!("{name}.json"));
        let data = fs::read_to_string(&path)?;
        serde_json::from_str(&data)
            .map_err(|e| crate::Error::session(format!("failed to parse state for {name}: {e}")))
    }

    /// List all services with persisted state.
    ///
    /// # Errors
    ///
    /// Returns an error if the state directory can't be read.
    pub fn list_services(&self) -> crate::Result<Vec<ServiceState>> {
        let state_dir = self.root.join("state");
        if !state_dir.exists() {
            return Ok(Vec::new());
        }

        let mut services = Vec::new();
        for entry in fs::read_dir(state_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let data = fs::read_to_string(&path)?;
                if let Ok(state) = serde_json::from_str::<ServiceState>(&data) {
                    services.push(state);
                }
            }
        }
        Ok(services)
    }

    /// Append a line to a service's log file.
    ///
    /// # Errors
    ///
    /// Returns an error if the log file can't be written.
    pub fn append_log(&self, name: &str, line: &str) -> crate::Result<()> {
        use std::io::Write;
        let path = self.root.join("logs").join(format!("{name}.log"));
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// Get the log file path for a service.
    #[must_use]
    pub fn log_path(&self, name: &str) -> PathBuf {
        self.root.join("logs").join(format!("{name}.log"))
    }

    /// Get the session root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Remove the session directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory can't be removed.
    pub fn cleanup(&self) -> crate::Result<()> {
        if self.root.exists() {
            fs::remove_dir_all(&self.root)?;
        }
        Ok(())
    }

    fn load_info(root: &Path) -> crate::Result<SessionInfo> {
        let data = fs::read_to_string(root.join("session.json"))?;
        serde_json::from_str(&data)
            .map_err(|e| crate::Error::session(format!("corrupt session.json: {e}")))
    }
}

/// Compute the session directory for a project path.
fn session_dir(project_path: &Path) -> PathBuf {
    let canonical = project_path
        .canonicalize()
        .unwrap_or_else(|_| project_path.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let hash = hex::encode(hasher.finalize());
    let short_hash = &hash[..16];

    // Use .cuenv/run/ relative to the project root
    project_path.join(".cuenv").join("run").join(short_hash)
}

/// Check if a process with the given PID is alive.
fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Signal 0 checks process existence without sending a signal
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false // Conservative: assume dead on non-unix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_dir_is_deterministic() {
        let path = Path::new("/tmp/test-project");
        let dir1 = session_dir(path);
        let dir2 = session_dir(path);
        assert_eq!(dir1, dir2);
    }

    #[test]
    fn test_session_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path();

        let session = SessionManager::create(project_path, "test-project").unwrap();
        let info = session.info().unwrap();
        assert_eq!(info.project_name, "test-project");
        assert_eq!(info.version, 1);
        assert_eq!(info.controller_pid, std::process::id());

        // Update service state
        let state = ServiceState {
            name: "db".to_string(),
            lifecycle: ServiceLifecycle::Ready,
            pid: Some(12345),
            started_at: Some(Utc::now()),
            ready_at: Some(Utc::now()),
            restarts: 0,
            exit_code: None,
            error: None,
        };
        session.update_service(&state).unwrap();

        // Read back
        let loaded = session.read_service("db").unwrap();
        assert_eq!(loaded.name, "db");
        assert_eq!(loaded.lifecycle, ServiceLifecycle::Ready);
        assert_eq!(loaded.pid, Some(12345));

        // List services
        let all = session.list_services().unwrap();
        assert_eq!(all.len(), 1);

        // Append log
        session.append_log("db", "starting postgres").unwrap();
        session.append_log("db", "ready to accept connections").unwrap();
        let log_content = fs::read_to_string(session.log_path("db")).unwrap();
        assert!(log_content.contains("starting postgres"));
        assert!(log_content.contains("ready to accept connections"));

        // Cleanup
        session.cleanup().unwrap();
        assert!(!session.root().exists());
    }

    #[test]
    fn test_is_pid_alive_self() {
        // Our own PID should be alive
        assert!(is_pid_alive(std::process::id()));
    }

    #[test]
    fn test_is_pid_alive_dead() {
        // PID 999999 is almost certainly not running
        assert!(!is_pid_alive(999_999));
    }

    #[test]
    fn test_load_nonexistent_session() {
        let dir = tempfile::tempdir().unwrap();
        let result = SessionManager::load(dir.path());
        assert!(result.is_err());
    }
}
