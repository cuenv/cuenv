//! Centralized path management for cuenv data directories.
//!
//! This module provides platform-appropriate paths following OS conventions:
//!
//! | Platform | State Dir | Cache Dir | Runtime Dir |
//! |----------|-----------|-----------|-------------|
//! | **macOS** | `~/Library/Application Support/cuenv` | `~/Library/Caches/cuenv` | `$TMPDIR` |
//! | **Linux** | `~/.local/state/cuenv` (XDG_STATE_HOME) | `~/.cache/cuenv` (XDG_CACHE_HOME) | `/run/user/$UID` (XDG_RUNTIME_DIR) |
//! | **Windows** | `%APPDATA%\cuenv` | `%LOCALAPPDATA%\cuenv` | `%TEMP%` |
//!
//! All functions support environment variable overrides for testing and CI:
//! - `CUENV_STATE_DIR` - Override state directory
//! - `CUENV_CACHE_DIR` - Override cache directory
//! - `CUENV_RUNTIME_DIR` - Override runtime directory

use crate::{Error, Result};
use std::path::PathBuf;

/// Get the state directory for persistent cuenv data.
///
/// State data includes:
/// - Hook execution state (`state/`)
/// - Approval records (`approved.json`)
///
/// Resolution order:
/// 1. `CUENV_STATE_DIR` environment variable
/// 2. Platform state directory + `/cuenv`
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
pub fn state_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CUENV_STATE_DIR")
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }

    // Use dirs::state_dir() which returns:
    // - Linux: XDG_STATE_HOME (~/.local/state)
    // - macOS: ~/Library/Application Support
    // - Windows: {FOLDERID_RoamingAppData}
    //
    // Note: state_dir() returns None on macOS/Windows, so we fall back to data_dir()
    let base = dirs::state_dir()
        .or_else(dirs::data_dir)
        .ok_or_else(|| Error::configuration("Could not determine state directory"))?;

    Ok(base.join("cuenv"))
}

/// Get the cache directory for cuenv.
///
/// Cache data includes:
/// - Task execution cache (`tasks/`)
/// - Task result metadata
///
/// Resolution order:
/// 1. `CUENV_CACHE_DIR` environment variable
/// 2. Platform cache directory + `/cuenv`
///
/// # Errors
///
/// Returns an error if the cache directory cannot be determined.
pub fn cache_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CUENV_CACHE_DIR")
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }

    let base = dirs::cache_dir()
        .ok_or_else(|| Error::configuration("Could not determine cache directory"))?;

    Ok(base.join("cuenv"))
}

/// Get the runtime directory for ephemeral cuenv data.
///
/// Runtime data includes:
/// - Coordinator socket
/// - PID files
/// - Lock files
///
/// Resolution order:
/// 1. `CUENV_RUNTIME_DIR` environment variable
/// 2. Platform runtime directory (or temp directory as fallback)
///
/// # Errors
///
/// Returns an error if the runtime directory cannot be determined.
pub fn runtime_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CUENV_RUNTIME_DIR")
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }

    // runtime_dir() returns:
    // - Linux: XDG_RUNTIME_DIR (/run/user/$UID)
    // - macOS/Windows: None (we use temp_dir as fallback)
    let base = dirs::runtime_dir().unwrap_or_else(std::env::temp_dir);

    Ok(base.join("cuenv"))
}

/// Get the path to the hook state directory.
///
/// This is where hook execution state files are stored.
pub fn hook_state_dir() -> Result<PathBuf> {
    Ok(state_dir()?.join("state"))
}

/// Get the path to the approvals file.
///
/// This file tracks which configurations have been approved for hook execution.
pub fn approvals_file() -> Result<PathBuf> {
    Ok(state_dir()?.join("approved.json"))
}

/// Get the path to the task cache directory.
///
/// This is where hermetic task execution results are cached.
pub fn task_cache_dir() -> Result<PathBuf> {
    Ok(cache_dir()?.join("tasks"))
}

/// Get the path to the coordinator socket.
///
/// The coordinator socket enables multi-UI support (CLI, TUI, Web).
pub fn coordinator_socket() -> Result<PathBuf> {
    Ok(runtime_dir()?.join("coordinator.sock"))
}

/// Get the path to the coordinator PID file.
pub fn coordinator_pid() -> Result<PathBuf> {
    Ok(runtime_dir()?.join("coordinator.pid"))
}

/// Get the path to the coordinator lock file.
pub fn coordinator_lock() -> Result<PathBuf> {
    Ok(runtime_dir()?.join("coordinator.lock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_dir_default() {
        // Clear override to test default behavior
        temp_env::with_var_unset("CUENV_STATE_DIR", || {
            let dir = state_dir().expect("state_dir should succeed");
            assert!(dir.ends_with("cuenv"), "Should end with cuenv: {:?}", dir);
        });
    }

    #[test]
    fn test_state_dir_override() {
        let test_dir = "/tmp/cuenv-test-state";
        temp_env::with_var("CUENV_STATE_DIR", Some(test_dir), || {
            let dir = state_dir().expect("state_dir should succeed");
            assert_eq!(dir, PathBuf::from(test_dir));
        });
    }

    #[test]
    fn test_cache_dir_default() {
        temp_env::with_var_unset("CUENV_CACHE_DIR", || {
            let dir = cache_dir().expect("cache_dir should succeed");
            assert!(dir.ends_with("cuenv"), "Should end with cuenv: {:?}", dir);
        });
    }

    #[test]
    fn test_cache_dir_override() {
        let test_dir = "/tmp/cuenv-test-cache";
        temp_env::with_var("CUENV_CACHE_DIR", Some(test_dir), || {
            let dir = cache_dir().expect("cache_dir should succeed");
            assert_eq!(dir, PathBuf::from(test_dir));
        });
    }

    #[test]
    fn test_runtime_dir_default() {
        temp_env::with_var_unset("CUENV_RUNTIME_DIR", || {
            let dir = runtime_dir().expect("runtime_dir should succeed");
            assert!(dir.ends_with("cuenv"), "Should end with cuenv: {:?}", dir);
        });
    }

    #[test]
    fn test_runtime_dir_override() {
        let test_dir = "/tmp/cuenv-test-runtime";
        temp_env::with_var("CUENV_RUNTIME_DIR", Some(test_dir), || {
            let dir = runtime_dir().expect("runtime_dir should succeed");
            assert_eq!(dir, PathBuf::from(test_dir));
        });
    }

    #[test]
    fn test_hook_state_dir() {
        temp_env::with_var_unset("CUENV_STATE_DIR", || {
            let dir = hook_state_dir().expect("hook_state_dir should succeed");
            assert!(dir.ends_with("state"), "Should end with state: {:?}", dir);
        });
    }

    #[test]
    fn test_approvals_file() {
        temp_env::with_var_unset("CUENV_STATE_DIR", || {
            let file = approvals_file().expect("approvals_file should succeed");
            assert!(
                file.ends_with("approved.json"),
                "Should end with approved.json: {:?}",
                file
            );
        });
    }

    #[test]
    fn test_task_cache_dir() {
        temp_env::with_var_unset("CUENV_CACHE_DIR", || {
            let dir = task_cache_dir().expect("task_cache_dir should succeed");
            assert!(dir.ends_with("tasks"), "Should end with tasks: {:?}", dir);
        });
    }

    #[test]
    fn test_coordinator_paths() {
        temp_env::with_var_unset("CUENV_RUNTIME_DIR", || {
            let socket = coordinator_socket().expect("coordinator_socket should succeed");
            let pid = coordinator_pid().expect("coordinator_pid should succeed");
            let lock = coordinator_lock().expect("coordinator_lock should succeed");

            assert!(
                socket.ends_with("coordinator.sock"),
                "Socket should end with coordinator.sock: {:?}",
                socket
            );
            assert!(
                pid.ends_with("coordinator.pid"),
                "PID should end with coordinator.pid: {:?}",
                pid
            );
            assert!(
                lock.ends_with("coordinator.lock"),
                "Lock should end with coordinator.lock: {:?}",
                lock
            );
        });
    }
}
