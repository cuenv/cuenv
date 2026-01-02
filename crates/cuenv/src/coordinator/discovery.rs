//! Coordinator discovery and lifecycle management.

// This module uses unsafe for libc::kill and env var manipulation in tests
#![allow(unsafe_code)]

use super::client::CoordinatorHandle;
use super::protocol::{MessageType, WireMessage};
use super::{lock_path, pid_path, socket_path};
use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;

/// Coordinator status.
#[derive(Debug, Clone)]
pub enum CoordinatorStatus {
    /// Coordinator is running and accepting connections.
    Running {
        /// Process ID.
        pid: u32,
        /// Socket path.
        socket: std::path::PathBuf,
    },
    /// Coordinator is not running.
    NotRunning,
    /// Socket exists but coordinator is not responding (stale).
    Stale {
        /// Stale socket path.
        socket: std::path::PathBuf,
    },
}

/// Detect whether a coordinator is running.
pub async fn detect_coordinator() -> CoordinatorStatus {
    let socket = socket_path();

    // Check if socket file exists
    if !socket.exists() {
        return CoordinatorStatus::NotRunning;
    }

    // Try to connect with timeout
    let connect_result = tokio::time::timeout(Duration::from_millis(500), try_ping(&socket)).await;

    match connect_result {
        Ok(Ok(pid)) => CoordinatorStatus::Running { pid, socket },
        Ok(Err(_)) | Err(_) => CoordinatorStatus::Stale { socket },
    }
}

/// Try to ping the coordinator and get its PID.
async fn try_ping(socket: &Path) -> io::Result<u32> {
    let mut stream = UnixStream::connect(socket).await?;

    // We need to register first before we can ping
    let client_id = uuid::Uuid::new_v4();
    let reg = WireMessage::register(
        client_id,
        super::protocol::ClientType::Producer {
            command: "_health_check".to_string(),
        },
        std::process::id(),
    );
    reg.write_to(&mut stream).await?;

    // Read registration ack
    let ack = WireMessage::read_from(&mut stream).await?;
    if ack.msg_type != MessageType::RegisterAck {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected registration ack",
        ));
    }

    // Read PID from file
    let pid_str = tokio::fs::read_to_string(pid_path()).await?;
    let pid: u32 = pid_str
        .trim()
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    Ok(pid)
}

/// Ensure a coordinator is running, starting one if necessary.
///
/// # Errors
///
/// Returns an error if coordinator detection or startup fails.
pub async fn ensure_coordinator_running() -> io::Result<CoordinatorHandle> {
    match detect_coordinator().await {
        CoordinatorStatus::Running { pid, socket } => Ok(CoordinatorHandle::existing(pid, socket)),
        CoordinatorStatus::NotRunning => start_coordinator().await,
        CoordinatorStatus::Stale { socket } => {
            cleanup_stale_coordinator(&socket).await?;
            start_coordinator().await
        }
    }
}

/// Check if a PID is a cuenv coordinator process.
/// This prevents accidentally killing unrelated processes if PID was reused.
#[cfg(unix)]
fn is_cuenv_process(pid: i32) -> bool {
    #[cfg(target_os = "linux")]
    {
        let cmdline_path = format!("/proc/{pid}/cmdline");
        if let Ok(cmdline) = std::fs::read_to_string(&cmdline_path) {
            return cmdline.contains("cuenv") && cmdline.contains("__coordinator");
        }
        false
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()
            .ok()
            .is_some_and(|o| {
                let cmd = String::from_utf8_lossy(&o.stdout);
                cmd.contains("cuenv") && cmd.contains("__coordinator")
            })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        // On other platforms, skip validation - less critical
        let _ = pid;
        true
    }
}

/// Clean up a stale coordinator.
async fn cleanup_stale_coordinator(socket: &Path) -> io::Result<()> {
    let pid_file = socket.with_extension("pid");

    // Try to read and kill stale process
    if let Ok(pid_str) = tokio::fs::read_to_string(&pid_file).await
        && let Ok(pid) = pid_str.trim().parse::<i32>()
    {
        // Verify process is actually a cuenv coordinator before killing
        #[cfg(unix)]
        if is_cuenv_process(pid) {
            // SAFETY: libc::kill with SIGTERM is safe after verifying PID ownership
            unsafe {
                let _ = libc::kill(pid, libc::SIGTERM);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    // Remove stale files
    let _ = tokio::fs::remove_file(socket).await;
    let _ = tokio::fs::remove_file(&pid_file).await;

    Ok(())
}

/// Start a new coordinator process.
async fn start_coordinator() -> io::Result<CoordinatorHandle> {
    let socket = socket_path();
    let lock = lock_path();

    // Ensure parent directory exists
    if let Some(parent) = socket.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Try to acquire lock (simple file-based locking)
    let _lock_guard = acquire_lock(&lock).await?;

    // Double-check after acquiring lock
    if let CoordinatorStatus::Running { pid, socket } = detect_coordinator().await {
        return Ok(CoordinatorHandle::existing(pid, socket));
    }

    // Start coordinator process
    let exe = std::env::current_exe()?;
    let child = Command::new(&exe)
        .arg("__coordinator")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;

    let pid = child.id().unwrap_or(0);

    // Wait for socket to be ready
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if socket.exists()
            && let CoordinatorStatus::Running { .. } = detect_coordinator().await
        {
            return Ok(CoordinatorHandle::new(pid, socket));
        }
    }

    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "coordinator failed to start",
    ))
}

/// Simple file-based lock.
async fn acquire_lock(lock_path: &Path) -> io::Result<LockGuard> {
    // Try to create the lock file exclusively
    for _ in 0..10 {
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
            .await
        {
            Ok(file) => {
                drop(file);
                return Ok(LockGuard {
                    path: lock_path.to_path_buf(),
                });
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                // Check if lock is stale (older than 30 seconds)
                if let Ok(meta) = tokio::fs::metadata(lock_path).await
                    && let Ok(modified) = meta.modified()
                    && modified.elapsed().unwrap_or(Duration::ZERO) > Duration::from_secs(30)
                {
                    let _ = tokio::fs::remove_file(lock_path).await;
                    continue;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => return Err(e),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "could not acquire lock",
    ))
}

/// RAII guard that removes the lock file on drop.
struct LockGuard {
    path: std::path::PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_detect_coordinator_not_running() {
        // Test that detect_coordinator returns NotRunning when no socket exists.
        // Uses the default socket_path() which typically won't exist in test env.
        // If CUENV_COORDINATOR_SOCKET env var is set externally, this test
        // may have different behavior, but that's acceptable for test isolation.
        let status = detect_coordinator().await;

        // In a clean test environment without a running coordinator,
        // we expect either NotRunning or Stale (if leftover socket exists)
        assert!(matches!(
            status,
            CoordinatorStatus::NotRunning | CoordinatorStatus::Stale { .. }
        ));
    }

    #[test]
    fn test_coordinator_status_variants() {
        // Test that all variants can be created and matched
        let running = CoordinatorStatus::Running {
            pid: 1234,
            socket: PathBuf::from("/tmp/test.sock"),
        };
        assert!(matches!(
            running,
            CoordinatorStatus::Running { pid: 1234, .. }
        ));

        let not_running = CoordinatorStatus::NotRunning;
        assert!(matches!(not_running, CoordinatorStatus::NotRunning));

        let stale = CoordinatorStatus::Stale {
            socket: PathBuf::from("/tmp/stale.sock"),
        };
        assert!(matches!(stale, CoordinatorStatus::Stale { .. }));
    }

    #[test]
    fn test_coordinator_status_debug() {
        let running = CoordinatorStatus::Running {
            pid: 5678,
            socket: PathBuf::from("/var/run/cuenv.sock"),
        };
        let debug_str = format!("{running:?}");
        assert!(debug_str.contains("Running"));
        assert!(debug_str.contains("5678"));

        let stale = CoordinatorStatus::Stale {
            socket: PathBuf::from("/tmp/stale.sock"),
        };
        let debug_str = format!("{stale:?}");
        assert!(debug_str.contains("Stale"));
    }

    #[test]
    fn test_coordinator_status_clone() {
        let running = CoordinatorStatus::Running {
            pid: 1000,
            socket: PathBuf::from("/tmp/clone.sock"),
        };
        let cloned = running.clone();
        assert!(matches!(
            cloned,
            CoordinatorStatus::Running { pid: 1000, .. }
        ));
    }

    #[test]
    fn test_lock_guard_drops_file() {
        let temp_dir = std::env::temp_dir();
        let lock_path = temp_dir.join(format!("test_lock_{}.lock", std::process::id()));

        // Create the lock file first
        std::fs::write(&lock_path, "locked").expect("could not create lock file");
        assert!(lock_path.exists());

        {
            let _guard = LockGuard {
                path: lock_path.clone(),
            };
            // Lock file still exists while guard is in scope
        }
        // After guard is dropped, lock file should be removed
        assert!(!lock_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_is_cuenv_process_for_nonexistent_pid() {
        // Test with a very high PID that's unlikely to exist
        let result = is_cuenv_process(99_999_999);
        // On macOS, ps will fail for nonexistent PIDs, returning false
        // On Linux, /proc/99999999/cmdline won't exist, returning false
        assert!(!result);
    }

    #[cfg(unix)]
    #[test]
    fn test_is_cuenv_process_for_current_process() {
        // Current process is not a cuenv coordinator (it's the test runner)
        #[allow(clippy::cast_possible_wrap)]
        let pid = std::process::id() as i32;
        let result = is_cuenv_process(pid);
        // Should return false since test runner is not cuenv __coordinator
        assert!(!result);
    }
}
