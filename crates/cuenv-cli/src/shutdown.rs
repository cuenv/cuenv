//! Graceful shutdown coordination for the cuenv CLI

use std::sync::Arc;
use tokio::sync::Notify;
use tracing::{info, warn};

/// Coordinates graceful shutdown across the application
#[derive(Debug, Clone)]
pub struct ShutdownCoordinator {
    notify: Arc<Notify>,
}

impl ShutdownCoordinator {
    /// Create a new shutdown coordinator
    pub fn new() -> Self {
        Self {
            notify: Arc::new(Notify::new()),
        }
    }

    /// Trigger a shutdown
    pub fn shutdown(&self) {
        info!("Shutdown triggered");
        self.notify.notify_waiters();
    }

    /// Wait for shutdown signal
    pub async fn wait_for_shutdown(&self) {
        self.notify.notified().await;
    }
}

impl Default for ShutdownCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

/// Install signal handlers for graceful shutdown
///
/// Returns a `ShutdownCoordinator` that will be notified when SIGTERM or SIGINT is received
pub fn install_signal_handlers() -> ShutdownCoordinator {
    let coordinator = ShutdownCoordinator::new();
    let coordinator_clone = coordinator.clone();

    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};

            let mut sigterm = signal(SignalKind::terminate())
                .expect("Failed to install SIGTERM handler");
            let mut sigint = signal(SignalKind::interrupt())
                .expect("Failed to install SIGINT handler");

            tokio::select! {
                _ = sigterm.recv() => {
                    info!("Received SIGTERM, initiating graceful shutdown");
                }
                _ = sigint.recv() => {
                    info!("Received SIGINT, initiating graceful shutdown");
                }
            }
        }

        #[cfg(windows)]
        {
            use tokio::signal::windows;

            let mut ctrl_c = windows::ctrl_c()
                .expect("Failed to install Ctrl+C handler");
            let mut ctrl_break = windows::ctrl_break()
                .expect("Failed to install Ctrl+Break handler");

            tokio::select! {
                _ = ctrl_c.recv() => {
                    info!("Received Ctrl+C, initiating graceful shutdown");
                }
                _ = ctrl_break.recv() => {
                    info!("Received Ctrl+Break, initiating graceful shutdown");
                }
            }
        }

        coordinator_clone.shutdown();
    });

    coordinator
}

/// Guard that ensures cleanup on drop
pub struct CleanupGuard<F: FnOnce()> {
    cleanup: Option<F>,
}

impl<F: FnOnce()> CleanupGuard<F> {
    /// Create a new cleanup guard with the given cleanup function
    pub fn new(cleanup: F) -> Self {
        Self {
            cleanup: Some(cleanup),
        }
    }

    /// Explicitly run the cleanup and consume the guard
    pub fn cleanup(mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}

impl<F: FnOnce()> Drop for CleanupGuard<F> {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            warn!("Running cleanup on drop");
            cleanup();
        }
    }
}
