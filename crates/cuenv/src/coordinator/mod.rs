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
//! │           Platform-specific runtime directory/coordinator.sock          │
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
///
/// Uses platform-appropriate paths via `cuenv_core::paths`:
/// - Linux: `$XDG_RUNTIME_DIR/cuenv/coordinator.sock`
/// - macOS: `$TMPDIR/cuenv/coordinator.sock`
/// - Windows: `%TEMP%\cuenv\coordinator.sock`
///
/// Can be overridden with `CUENV_COORDINATOR_SOCKET` environment variable.
#[must_use]
pub fn socket_path() -> PathBuf {
    // Check for override via environment variable
    if let Ok(socket) = std::env::var("CUENV_COORDINATOR_SOCKET")
        && !socket.is_empty()
    {
        return PathBuf::from(socket);
    }

    cuenv_core::paths::coordinator_socket()
        .unwrap_or_else(|_| PathBuf::from("/tmp/cuenv/coordinator.sock"))
}

/// Get the path to the coordinator PID file.
#[must_use]
pub fn pid_path() -> PathBuf {
    cuenv_core::paths::coordinator_pid()
        .unwrap_or_else(|_| PathBuf::from("/tmp/cuenv/coordinator.pid"))
}

/// Get the path to the coordinator lock file.
#[must_use]
pub fn lock_path() -> PathBuf {
    cuenv_core::paths::coordinator_lock()
        .unwrap_or_else(|_| PathBuf::from("/tmp/cuenv/coordinator.lock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socket_path() {
        let path = socket_path();
        assert!(path.to_string_lossy().contains("coordinator.sock"));
    }

    #[test]
    fn test_pid_path() {
        let path = pid_path();
        assert!(path.to_string_lossy().contains("coordinator.pid"));
    }

    #[test]
    fn test_lock_path() {
        let path = lock_path();
        assert!(path.to_string_lossy().contains("coordinator.lock"));
    }
}
