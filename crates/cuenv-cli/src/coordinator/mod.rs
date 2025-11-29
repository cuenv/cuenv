//! Event Coordinator for multi-UI support.
//!
//! The `EventCoordinator` is a Unix Domain Socket server that enables multiple
//! UI frontends (CLI, TUI, Web) to subscribe to a unified event stream.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │                        EventCoordinator (UDS Server)                    │
//! │                    ~/.cuenv/coordinator.sock                            │
//! └─────────────────────────────────────────────────────────────────────────┘
//!                    │                              │
//!         ┌──────────┘                              └──────────┐
//!         v                                                    v
//! ┌───────────────┐                                    ┌───────────────┐
//! │  CLI Producer │                                    │  TUI Consumer │
//! │  (events out) │                                    │  (events in)  │
//! └───────────────┘                                    └───────────────┘
//! ```

pub mod client;
pub mod discovery;
pub mod protocol;
pub mod server;

use std::path::PathBuf;

/// Get the path to the coordinator socket.
#[must_use]
pub fn socket_path() -> PathBuf {
    // Check for override via environment variable
    if let Ok(socket) = std::env::var("CUENV_COORDINATOR_SOCKET") {
        return PathBuf::from(socket);
    }

    // Use XDG runtime directory if available, otherwise ~/.cuenv
    dirs::runtime_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".cuenv")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("cuenv-coordinator.sock")
}

/// Get the path to the coordinator PID file.
#[must_use]
pub fn pid_path() -> PathBuf {
    socket_path().with_extension("pid")
}

/// Get the path to the coordinator lock file.
#[must_use]
pub fn lock_path() -> PathBuf {
    socket_path().with_extension("lock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socket_path() {
        let path = socket_path();
        assert!(path.to_string_lossy().contains("cuenv-coordinator.sock"));
    }

    #[test]
    fn test_pid_path() {
        let path = pid_path();
        assert!(path.to_string_lossy().contains("cuenv-coordinator.pid"));
    }

    #[test]
    fn test_lock_path() {
        let path = lock_path();
        assert!(path.to_string_lossy().contains("cuenv-coordinator.lock"));
    }
}
