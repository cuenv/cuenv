//! Session state management for running services.
//!
//! Persists service state under `.cuenv/run/<project-hash>/` for
//! `cuenv ps`, `cuenv logs`, `cuenv down`, and `cuenv restart`.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use cuenv_core::manifest::ServicePort;

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
    /// Declared ports for this service (from CUE definition).
    #[serde(default)]
    pub ports: Vec<ServicePort>,
}

/// Manages session state on disk.
pub struct SessionManager {
    root: PathBuf,
}

/// Result of asking a persisted service session to shut down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownRequestOutcome {
    /// The controller was still running and received the shutdown signal.
    Signaled {
        /// PID of the `cuenv up` controller process.
        controller_pid: u32,
    },
    /// The persisted controller PID no longer exists.
    Stale {
        /// PID recorded in the persisted session.
        controller_pid: u32,
    },
}

#[derive(Debug, Clone, Copy)]
enum ServiceControlRequest {
    Restart,
    Stop,
}

impl ServiceControlRequest {
    fn directory(self) -> &'static str {
        match self {
            Self::Restart => "restart",
            Self::Stop => "stop",
        }
    }
}

impl SessionManager {
    /// Create a new session for a project, writing `session.json`.
    ///
    /// Fails if an existing session is corrupt or its controller PID is still alive.
    ///
    /// # Errors
    ///
    /// Returns an error if the session metadata is corrupt, the session directory
    /// can't be created, or a live session exists.
    pub fn create(project_path: &Path, project_name: &str) -> crate::Result<Self> {
        let root = session_dir(project_path);

        // Check for existing live session
        if root.join("session.json").exists() {
            let existing = Self::load_info(&root)?;
            if is_pid_alive(existing.controller_pid) {
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
        fs::create_dir_all(root.join("control").join("restart"))?;
        fs::create_dir_all(root.join("control").join("stop"))?;

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
            .is_ok_and(|info| is_pid_alive(info.controller_pid))
    }

    /// Request graceful shutdown of the session controller.
    ///
    /// # Errors
    ///
    /// Returns an error if session metadata cannot be read or the controller
    /// process cannot be signalled.
    pub fn request_shutdown(&self) -> crate::Result<ShutdownRequestOutcome> {
        let info = self.info()?;
        if !is_pid_alive(info.controller_pid) {
            return Ok(ShutdownRequestOutcome::Stale {
                controller_pid: info.controller_pid,
            });
        }

        signal_controller_shutdown(info.controller_pid)?;
        Ok(ShutdownRequestOutcome::Signaled {
            controller_pid: info.controller_pid,
        })
    }

    /// Queue a restart request for a running service supervisor.
    ///
    /// # Errors
    ///
    /// Returns an error if the control directory or request marker cannot be written.
    pub fn request_service_restart(&self, name: &str) -> crate::Result<()> {
        self.request_service_control(ServiceControlRequest::Restart, name)
    }

    /// Consume a queued restart request for a service.
    ///
    /// # Errors
    ///
    /// Returns an error if the request marker exists but cannot be removed.
    pub fn take_service_restart_request(&self, name: &str) -> crate::Result<bool> {
        self.take_service_control_request(ServiceControlRequest::Restart, name)
    }

    /// Queue a stop request for a running service supervisor.
    ///
    /// # Errors
    ///
    /// Returns an error if the control directory or request marker cannot be written.
    pub fn request_service_stop(&self, name: &str) -> crate::Result<()> {
        self.request_service_control(ServiceControlRequest::Stop, name)
    }

    /// Consume a queued stop request for a service.
    ///
    /// # Errors
    ///
    /// Returns an error if the request marker exists but cannot be removed.
    pub fn take_service_stop_request(&self, name: &str) -> crate::Result<bool> {
        self.take_service_control_request(ServiceControlRequest::Stop, name)
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
        let info: SessionInfo = serde_json::from_str(&data)
            .map_err(|e| crate::Error::session(format!("corrupt session.json: {e}")))?;
        if info.controller_pid == 0 {
            return Err(crate::Error::session(
                "corrupt session.json: controller PID must be greater than zero",
            ));
        }
        #[cfg(unix)]
        if i32::try_from(info.controller_pid).is_err() {
            return Err(crate::Error::session(format!(
                "corrupt session.json: controller PID {} does not fit platform pid_t",
                info.controller_pid
            )));
        }
        Ok(info)
    }

    fn request_service_control(
        &self,
        request: ServiceControlRequest,
        name: &str,
    ) -> crate::Result<()> {
        fs::create_dir_all(self.control_request_dir(request))?;
        fs::write(
            self.control_request_path(request, name),
            Utc::now().to_rfc3339(),
        )?;
        Ok(())
    }

    fn take_service_control_request(
        &self,
        request: ServiceControlRequest,
        name: &str,
    ) -> crate::Result<bool> {
        match fs::remove_file(self.control_request_path(request, name)) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn control_request_dir(&self, request: ServiceControlRequest) -> PathBuf {
        self.root.join("control").join(request.directory())
    }

    fn control_request_path(&self, request: ServiceControlRequest, name: &str) -> PathBuf {
        self.control_request_dir(request)
            .join(format!("{name}.request"))
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
///
/// On Unix, only `ESRCH` establishes that the process is absent. Other probe
/// errors cannot establish that removing its session is safe.
fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    #[cfg(unix)]
    {
        let Ok(raw_pid) = i32::try_from(pid) else {
            return false;
        };
        #[expect(
            unsafe_code,
            reason = "kill(pid, 0) checks process liveness without sending a signal"
        )]
        // SAFETY: kill takes integer arguments and touches no Rust memory;
        // signal 0 checks process existence without sending a signal.
        let status = unsafe { libc::kill(raw_pid, 0) };
        let result = if status == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        };
        pid_probe_indicates_alive(result)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false // Conservative: assume dead on non-unix
    }
}

#[cfg(unix)]
fn pid_probe_indicates_alive(result: std::io::Result<()>) -> bool {
    match result {
        Ok(()) => true,
        Err(error) => error.raw_os_error() != Some(libc::ESRCH),
    }
}

fn signal_controller_shutdown(pid: u32) -> crate::Result<()> {
    if pid == 0 {
        return Err(crate::Error::session("invalid controller PID 0"));
    }

    #[cfg(unix)]
    {
        let raw_pid = i32::try_from(pid).map_err(|_| crate::Error::Session {
            message: format!("controller PID {pid} does not fit platform pid_t"),
            help: None,
        })?;

        #[expect(
            unsafe_code,
            reason = "Sending SIGINT to the recorded controller PID requests graceful shutdown"
        )]
        // SAFETY: kill takes integer arguments and touches no Rust memory;
        // raw_pid is positive and fits the platform PID type.
        let result = unsafe { libc::kill(raw_pid, libc::SIGINT) };

        if result == 0 {
            Ok(())
        } else {
            Err(crate::Error::Session {
                message: format!(
                    "failed to signal controller PID {pid}: {}",
                    std::io::Error::last_os_error()
                ),
                help: Some("Check `cuenv ps` or remove the stale .cuenv/run session".into()),
            })
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Err(crate::Error::Session {
            message: "service shutdown signalling is not supported on this platform".to_string(),
            help: None,
        })
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
            ports: vec![],
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
        session
            .append_log("db", "ready to accept connections")
            .unwrap();
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

    #[cfg(unix)]
    #[test]
    fn only_esrch_proves_a_controller_process_is_absent() {
        assert!(pid_probe_indicates_alive(Ok(())));
        assert!(!pid_probe_indicates_alive(Err(
            std::io::Error::from_raw_os_error(libc::ESRCH)
        )));
        assert!(pid_probe_indicates_alive(Err(
            std::io::Error::from_raw_os_error(libc::EPERM)
        )));
        assert!(pid_probe_indicates_alive(Err(
            std::io::Error::from_raw_os_error(libc::EINVAL)
        )));
        assert!(pid_probe_indicates_alive(Err(std::io::Error::other(
            "probe failed without an OS error code"
        ))));
    }

    #[test]
    fn test_load_nonexistent_session() {
        let dir = tempfile::tempdir().unwrap();
        let result = SessionManager::load(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_request_shutdown_reports_stale_controller() {
        let dir = tempfile::tempdir().unwrap();
        let session = SessionManager::create(dir.path(), "test-project").unwrap();
        let mut info = session.info().unwrap();
        info.controller_pid = 999_999;
        let json = serde_json::to_string_pretty(&info).unwrap();
        fs::write(session.root().join("session.json"), json).unwrap();

        let outcome = session.request_shutdown().unwrap();
        assert_eq!(
            outcome,
            ShutdownRequestOutcome::Stale {
                controller_pid: 999_999
            }
        );
    }

    fn overwrite_controller_pid(session: &SessionManager, pid: u32) {
        let mut info = session.info().expect("read session metadata");
        info.controller_pid = pid;
        let json = serde_json::to_string_pretty(&info).expect("serialize session metadata");
        fs::write(session.root().join("session.json"), json).expect("write session metadata");
    }

    #[test]
    fn zero_controller_pid_is_corrupt_session_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let session = SessionManager::create(dir.path(), "test-project").unwrap();
        overwrite_controller_pid(&session, 0);

        let error = session.info().expect_err("zero PID is invalid");
        assert!(error.to_string().contains("corrupt session.json"));
        assert!(error.to_string().contains("controller PID"));
        assert!(!session.is_alive());
        assert!(session.root().join("session.json").exists());
    }

    #[test]
    fn creating_a_session_preserves_corrupt_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let session = SessionManager::create(dir.path(), "test-project").unwrap();
        let mut info = session.info().unwrap();
        info.controller_pid = 0;
        let zero_pid = serde_json::to_string(&info).unwrap();

        for metadata in [zero_pid, "{invalid json".to_string()] {
            let path = session.root().join("session.json");
            fs::write(&path, &metadata).unwrap();
            let Err(error) = SessionManager::create(dir.path(), "replacement") else {
                panic!("corrupt session must not be replaced");
            };
            assert!(error.to_string().contains("corrupt session.json"));
            assert_eq!(fs::read_to_string(&path).unwrap(), metadata);
        }
    }

    #[test]
    fn creating_a_session_still_removes_a_stale_session() {
        let dir = tempfile::tempdir().unwrap();
        let session = SessionManager::create(dir.path(), "test-project").unwrap();
        let stale_marker = session.root().join("stale-marker");
        fs::write(&stale_marker, "old session").unwrap();

        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--list",
                "--exact",
                "session::tests::creating_a_session_still_removes_a_stale_session",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn a controller process");
        let pid = child.id();
        assert!(child.wait().expect("reap controller process").success());
        overwrite_controller_pid(&session, pid);

        let replacement = SessionManager::create(dir.path(), "replacement").unwrap();
        assert!(!stale_marker.exists());
        assert_eq!(
            replacement.info().unwrap().controller_pid,
            std::process::id()
        );
        assert_eq!(replacement.info().unwrap().project_name, "replacement");
    }

    #[cfg(unix)]
    #[test]
    fn out_of_range_controller_pid_is_preserved_as_corrupt_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let session = SessionManager::create(dir.path(), "test-project").unwrap();
        overwrite_controller_pid(&session, u32::MAX);
        let path = session.root().join("session.json");
        let metadata = fs::read_to_string(&path).unwrap();

        let error = session.request_shutdown().expect_err("PID must fit pid_t");
        assert!(error.to_string().contains("corrupt session.json"));
        assert!(error.to_string().contains("does not fit platform pid_t"));
        assert!(!session.is_alive());
        let Err(error) = SessionManager::create(dir.path(), "replacement") else {
            panic!("out-of-range PID must not be treated as a stale session");
        };
        assert!(error.to_string().contains("corrupt session.json"));
        assert_eq!(fs::read_to_string(path).unwrap(), metadata);
    }

    #[cfg(unix)]
    #[test]
    fn zero_controller_pid_cannot_signal_process_group() {
        use std::os::unix::process::CommandExt;

        const CHILD_MARKER: &str = "CUENV_TEST_ZERO_CONTROLLER_PID_CHILD";
        if std::env::var_os(CHILD_MARKER).is_some() {
            // SAFETY: getpgrp has no arguments and does not access Rust memory.
            #[expect(unsafe_code, reason = "verifying the test subprocess process group")]
            let process_group = unsafe { libc::getpgrp() };
            assert_eq!(
                process_group,
                i32::try_from(std::process::id()).unwrap(),
                "the signalling regression must run in an isolated process group"
            );

            let dir = tempfile::tempdir().unwrap();
            let session = SessionManager::create(dir.path(), "test-project").unwrap();
            overwrite_controller_pid(&session, 0);
            let loaded = SessionManager::load(dir.path()).expect("load session directory");

            let error = loaded
                .request_shutdown()
                .expect_err("zero controller PID must be rejected");
            assert!(error.to_string().contains("corrupt session.json"));
            assert!(loaded.root().join("session.json").exists());
            assert!(!is_pid_alive(0));
            assert!(signal_controller_shutdown(0).is_err());
            assert!(!is_pid_alive(u32::MAX));
            assert!(signal_controller_shutdown(u32::MAX).is_err());
            return;
        }

        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("session::tests::zero_controller_pid_cannot_signal_process_group")
            .arg("--nocapture")
            .env(CHILD_MARKER, "1")
            .process_group(0)
            .output()
            .expect("run isolated signalling regression");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "isolated signalling regression failed: {stdout}\n{stderr}"
        );
        assert!(stdout.contains("running 1 test"), "{stdout}\n{stderr}");
    }

    #[test]
    fn test_service_restart_request_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let session = SessionManager::create(dir.path(), "test-project").unwrap();

        assert!(!session.take_service_restart_request("db").unwrap());

        session.request_service_restart("db").unwrap();
        assert!(session.take_service_restart_request("db").unwrap());
        assert!(!session.take_service_restart_request("db").unwrap());
    }

    #[test]
    fn test_service_stop_request_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let session = SessionManager::create(dir.path(), "test-project").unwrap();

        assert!(!session.take_service_stop_request("db").unwrap());

        session.request_service_stop("db").unwrap();
        assert!(session.take_service_stop_request("db").unwrap());
        assert!(!session.take_service_stop_request("db").unwrap());
    }
}
