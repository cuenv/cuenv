//! Global process registry for tracking and terminating spawned child processes.
//!
//! This module provides a centralized registry for tracking PIDs of child processes
//! spawned during task execution. When the application receives a termination signal
//! (e.g., Ctrl-C), the registry can terminate all tracked processes and their children.
//!
//! # Process Groups
//!
//! On Unix systems, spawned processes are placed in their own process groups using
//! `setpgid(0, 0)`. This allows terminating the entire process tree (including any
//! child processes spawned by the task) by sending signals to the process group.
//!
//! # Usage
//!
//! ```ignore
//! use cuenv_core::tasks::process_registry::global_registry;
//!
//! // Register a process after spawning
//! if let Some(pid) = child.id() {
//!     global_registry().register(pid, "task_name".to_string()).await;
//! }
//!
//! // Unregister when process completes
//! global_registry().unregister(pid).await;
//!
//! // Terminate all on shutdown
//! global_registry().terminate_all(Duration::from_secs(5)).await;
//! ```

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Global process registry singleton.
static GLOBAL_REGISTRY: OnceLock<Arc<ProcessRegistry>> = OnceLock::new();

/// Returns the global process registry instance.
///
/// The registry is lazily initialized on first access and shared across
/// the entire application.
#[must_use]
pub fn global_registry() -> Arc<ProcessRegistry> {
    GLOBAL_REGISTRY
        .get_or_init(|| Arc::new(ProcessRegistry::new()))
        .clone()
}

/// Registry for tracking spawned child processes.
///
/// Maintains a mapping of PIDs to task names, allowing for graceful shutdown
/// of all child processes when the application is terminated.
pub struct ProcessRegistry {
    /// Map of process IDs to task names for debugging/logging.
    pids: Mutex<HashMap<u32, String>>,
}

impl ProcessRegistry {
    /// Creates a new empty process registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pids: Mutex::new(HashMap::new()),
        }
    }

    /// Registers a process with the given PID and task name.
    ///
    /// Call this immediately after spawning a child process.
    pub async fn register(&self, pid: u32, task_name: String) {
        let mut pids = self.pids.lock().await;
        debug!(pid, task = %task_name, "Registering process");
        pids.insert(pid, task_name);
    }

    /// Unregisters a process after it has completed.
    ///
    /// Call this after successfully waiting for the process to exit.
    pub async fn unregister(&self, pid: u32) {
        let mut pids = self.pids.lock().await;
        if let Some(task_name) = pids.remove(&pid) {
            debug!(pid, task = %task_name, "Unregistering process");
        }
    }

    /// Returns the number of currently tracked processes.
    pub async fn count(&self) -> usize {
        self.pids.lock().await.len()
    }

    /// Terminates all registered processes gracefully.
    ///
    /// This method:
    /// 1. Sends SIGTERM to all process groups (allowing graceful shutdown)
    /// 2. Waits up to `timeout` for processes to exit
    /// 3. Sends SIGKILL to any remaining processes
    ///
    /// On Unix, signals are sent to the entire process group (-pid) to ensure
    /// child processes spawned by tasks are also terminated.
    pub async fn terminate_all(&self, timeout: Duration) {
        let mut pids = self.pids.lock().await;

        if pids.is_empty() {
            return;
        }

        info!(count = pids.len(), "Terminating child processes");

        // Phase 1: Send SIGTERM to all process groups
        for (pid, task_name) in pids.iter() {
            debug!(pid, task = %task_name, "Sending SIGTERM");
            Self::send_term_signal(*pid);
        }

        // Phase 2: Wait for processes to exit (with timeout)
        let deadline = std::time::Instant::now() + timeout;
        while !pids.is_empty() && std::time::Instant::now() < deadline {
            // Check which processes have exited
            let mut exited = Vec::new();
            for (pid, _) in pids.iter() {
                if !Self::is_process_alive(*pid) {
                    exited.push(*pid);
                }
            }

            // Remove exited processes
            for pid in exited {
                if let Some(task_name) = pids.remove(&pid) {
                    debug!(pid, task = %task_name, "Process exited gracefully");
                }
            }

            if !pids.is_empty() {
                // Short sleep before checking again
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        // Phase 3: Force kill any remaining processes
        for (pid, task_name) in pids.drain() {
            warn!(pid, task = %task_name, "Force killing process after timeout");
            Self::send_kill_signal(pid);
        }
    }

    /// Sends SIGTERM to a process group (Unix) or terminates process (Windows).
    #[cfg(unix)]
    fn send_term_signal(pid: u32) {
        // SAFETY: libc::kill with negative pid sends signal to entire process group.
        // The pid was obtained from a spawned child process and is valid.
        // SIGTERM is a safe signal that requests graceful termination.
        #[expect(unsafe_code, reason = "Required for POSIX signal handling")]
        unsafe {
            // Use negative PID to send to entire process group
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
    }

    /// Sends SIGKILL to a process group (Unix) or terminates process (Windows).
    #[cfg(unix)]
    fn send_kill_signal(pid: u32) {
        // SAFETY: libc::kill with negative pid sends signal to entire process group.
        // The pid was obtained from a spawned child process and is valid.
        // SIGKILL forces immediate termination.
        #[expect(unsafe_code, reason = "Required for POSIX signal handling")]
        unsafe {
            // Use negative PID to kill entire process group
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }

    /// Checks if a process is still alive.
    #[cfg(unix)]
    fn is_process_alive(pid: u32) -> bool {
        // SAFETY: libc::kill with signal 0 checks if process exists without sending a signal.
        // This is a standard POSIX idiom for checking process existence.
        #[expect(unsafe_code, reason = "Required for POSIX process existence check")]
        unsafe {
            libc::kill(pid as i32, 0) == 0
        }
    }

    /// Windows implementation: terminate process using sysinfo crate.
    #[cfg(windows)]
    fn send_term_signal(pid: u32) {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System};

        let mut system = System::new();
        let process_pid = Pid::from(pid as usize);
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[process_pid]),
            false,
            ProcessRefreshKind::nothing(),
        );

        if let Some(process) = system.process(process_pid) {
            let _ = process.kill_with(Signal::Term);
        }
    }

    /// Windows implementation: force kill process.
    #[cfg(windows)]
    fn send_kill_signal(pid: u32) {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System};

        let mut system = System::new();
        let process_pid = Pid::from(pid as usize);
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[process_pid]),
            false,
            ProcessRefreshKind::nothing(),
        );

        if let Some(process) = system.process(process_pid) {
            let _ = process.kill_with(Signal::Kill);
        }
    }

    /// Windows implementation: check if process is alive.
    #[cfg(windows)]
    fn is_process_alive(pid: u32) -> bool {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

        let mut system = System::new();
        let process_pid = Pid::from(pid as usize);
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[process_pid]),
            false,
            ProcessRefreshKind::nothing(),
        );

        system.process(process_pid).is_some()
    }
}

impl Default for ProcessRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_registry_new() {
        let registry = ProcessRegistry::new();
        assert_eq!(registry.count().await, 0);
    }

    #[tokio::test]
    async fn test_register_unregister() {
        let registry = ProcessRegistry::new();

        registry.register(1234, "test_task".to_string()).await;
        assert_eq!(registry.count().await, 1);

        registry.unregister(1234).await;
        assert_eq!(registry.count().await, 0);
    }

    #[tokio::test]
    async fn test_unregister_nonexistent() {
        let registry = ProcessRegistry::new();

        // Should not panic when unregistering non-existent PID
        registry.unregister(9999).await;
        assert_eq!(registry.count().await, 0);
    }

    #[tokio::test]
    async fn test_terminate_empty() {
        let registry = ProcessRegistry::new();

        // Should return immediately when no processes are registered
        registry.terminate_all(Duration::from_secs(1)).await;
        assert_eq!(registry.count().await, 0);
    }

    #[tokio::test]
    async fn test_global_registry_singleton() {
        let r1 = global_registry();
        let r2 = global_registry();

        // Both should point to the same instance
        assert!(Arc::ptr_eq(&r1, &r2));
    }
}
