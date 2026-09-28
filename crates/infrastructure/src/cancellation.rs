//! Two-stage interruption shared by signal handlers, the engine and every
//! provider process it launches.
//!
//! Terraform's semantics, which cuenv follows:
//!
//! 1. The first interrupt ([`Cancellation::stop`]) stops starting new
//!    resources and asks every running provider to stop its in-flight
//!    operations (the plugin protocol's `Stop` procedure). Whatever those
//!    operations return is still recorded before the run ends.
//! 2. The second interrupt ([`Cancellation::terminate_providers`]) kills every
//!    provider process (and its process group) synchronously and removes
//!    their socket directories, so the command can exit at once. Before
//!    exiting, the command can wait briefly for a record being written
//!    ([`Cancellation::wait_for_recordings`]).
//!
//! Providers run in their own process group, so a Ctrl-C typed at the
//! terminal reaches cuenv only and cuenv decides what the providers see.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use crate::plugin::ProviderProcess;

/// A request to stop a run, shared between signal handlers and the engine.
///
/// Clones share state. Pass the same `Cancellation` to
/// [`crate::EngineOptions::cancellation`] and to the command's signal
/// handling.
#[derive(Debug, Clone, Default)]
pub struct Cancellation {
    shared: Arc<Shared>,
}

#[derive(Debug, Default)]
struct Shared {
    stop_requested: AtomicBool,
    terminated: AtomicBool,
    providers: Mutex<Vec<Weak<ProviderProcess>>>,
    socket_directories: Mutex<Vec<PathBuf>>,
    /// Records being written (to the state store or saved locally).
    recordings: Mutex<usize>,
    recording_finished: Condvar,
}

/// Marks a record being written; see [`Cancellation::begin_recording`].
/// Dropping it ends the recording.
#[derive(Debug)]
#[must_use = "the recording ends when the guard is dropped"]
pub struct RecordingGuard {
    shared: Arc<Shared>,
}

impl Drop for RecordingGuard {
    fn drop(&mut self) {
        let mut recordings = self
            .shared
            .recordings
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *recordings = recordings.saturating_sub(1);
        self.shared.recording_finished.notify_all();
    }
}

impl Cancellation {
    /// First interrupt: stop before the next resource and ask every running
    /// provider to stop its in-flight operations.
    ///
    /// The provider `Stop` procedures are sent from tasks on the current
    /// tokio runtime; without a runtime only the flag is set. Calling this
    /// more than once has no further effect.
    pub fn stop(&self) {
        if self.shared.stop_requested.swap(true, Ordering::SeqCst) {
            return;
        }
        let providers = self.live_providers();
        tracing::info!(
            providers = providers.len(),
            "stop requested: no new resource starts and providers are asked to stop"
        );
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::debug!(
                providers = providers.len(),
                "no async runtime; providers were not asked to stop"
            );
            return;
        };
        for provider in providers {
            runtime.spawn(async move { provider.request_stop().await });
        }
    }

    /// Whether [`Cancellation::stop`] was called.
    #[must_use]
    pub fn is_stop_requested(&self) -> bool {
        self.shared.stop_requested.load(Ordering::SeqCst)
    }

    /// Second interrupt: kill every provider process (with its process
    /// group) now, and remove their socket directories.
    ///
    /// Synchronous and safe to call from any thread, including a signal
    /// handling path that is about to exit the process. Providers launched
    /// afterwards are killed as soon as they start. Also implies
    /// [`Cancellation::stop`]'s flag, so no new resource is started.
    pub fn terminate_providers(&self) {
        self.shared.stop_requested.store(true, Ordering::SeqCst);
        self.shared.terminated.store(true, Ordering::SeqCst);
        let providers = self.live_providers();
        tracing::info!(providers = providers.len(), "terminating providers");
        for provider in providers {
            provider.kill();
        }
        self.remove_socket_directories();
    }

    /// Whether [`Cancellation::terminate_providers`] was called.
    #[must_use]
    pub fn is_terminated(&self) -> bool {
        self.shared.terminated.load(Ordering::SeqCst)
    }

    /// Remove every provider socket directory still registered. Called by
    /// [`Cancellation::terminate_providers`]; a normal exit removes each
    /// directory when its provider stops.
    pub fn remove_socket_directories(&self) {
        let directories = std::mem::take(
            &mut *self
                .shared
                .socket_directories
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for directory in directories {
            if let Err(error) = std::fs::remove_dir_all(&directory)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::debug!(directory = %directory.display(), %error, "socket directory not removed");
            }
        }
    }

    /// Number of provider socket directories still registered.
    #[must_use]
    pub fn socket_directory_count(&self) -> usize {
        self.shared
            .socket_directories
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Mark a record as being written until the returned guard is dropped,
    /// so a forced exit can wait for it ([`Cancellation::wait_for_recordings`]).
    pub fn begin_recording(&self) -> RecordingGuard {
        *self
            .shared
            .recordings
            .lock()
            .unwrap_or_else(PoisonError::into_inner) += 1;
        RecordingGuard {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Block up to `bound` until no record is being written. Returns whether
    /// none is. Meant for a forced exit, before releasing the lock: a write
    /// in flight finishes (or is saved locally) instead of being cut off.
    #[must_use]
    pub fn wait_for_recordings(&self, bound: Duration) -> bool {
        let deadline = Instant::now() + bound;
        let mut recordings = self
            .shared
            .recordings
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while *recordings > 0 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                tracing::warn!(
                    recordings = *recordings,
                    "records still being written when the wait ended"
                );
                return false;
            }
            recordings = self
                .shared
                .recording_finished
                .wait_timeout(recordings, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    /// Track a provider process so both stages can reach it. Called as soon
    /// as the process is spawned, before its handshake.
    pub(crate) fn register(&self, provider: &Arc<ProviderProcess>) {
        {
            let mut providers = self
                .shared
                .providers
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            providers.retain(|existing| existing.strong_count() > 0);
            providers.push(Arc::downgrade(provider));
        }
        if self.is_terminated() {
            provider.kill();
        }
    }

    /// Track a provider's socket directory so a forced exit removes it.
    pub(crate) fn register_socket_directory(&self, directory: &Path) {
        self.shared
            .socket_directories
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(directory.to_path_buf());
    }

    /// Stop tracking a socket directory its owner removed.
    pub(crate) fn unregister_socket_directory(&self, directory: &Path) {
        self.shared
            .socket_directories
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|registered| registered != directory);
    }

    /// Number of provider processes still running and tracked.
    #[must_use]
    pub fn live_provider_count(&self) -> usize {
        self.live_providers().len()
    }

    fn live_providers(&self) -> Vec<Arc<ProviderProcess>> {
        self.shared
            .providers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_and_terminate_set_their_flags() {
        let cancellation = Cancellation::default();
        let shared = cancellation.clone();
        assert!(!cancellation.is_stop_requested());
        shared.stop();
        assert!(cancellation.is_stop_requested());
        assert!(!cancellation.is_terminated());
        shared.terminate_providers();
        assert!(cancellation.is_terminated());
        assert_eq!(cancellation.live_provider_count(), 0);
    }

    #[test]
    fn terminate_alone_also_stops_new_work() {
        let cancellation = Cancellation::default();
        cancellation.terminate_providers();
        assert!(cancellation.is_stop_requested());
    }

    #[test]
    fn terminate_removes_registered_socket_directories() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("cuenv-plugin-test");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("plugin.sock"), b"").unwrap();
        let cancellation = Cancellation::default();
        cancellation.register_socket_directory(&directory);
        assert_eq!(cancellation.socket_directory_count(), 1);
        cancellation.terminate_providers();
        assert!(!directory.exists());
        assert_eq!(cancellation.socket_directory_count(), 0);

        let kept = root.path().join("kept");
        std::fs::create_dir(&kept).unwrap();
        cancellation.register_socket_directory(&kept);
        cancellation.unregister_socket_directory(&kept);
        cancellation.remove_socket_directories();
        assert!(kept.exists());
    }

    #[test]
    fn a_forced_exit_waits_for_a_record_in_flight() {
        let cancellation = Cancellation::default();
        assert!(cancellation.wait_for_recordings(Duration::ZERO));
        let guard = cancellation.begin_recording();
        assert!(!cancellation.wait_for_recordings(Duration::from_millis(20)));
        let finisher = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            drop(guard);
        });
        let started = Instant::now();
        assert!(cancellation.wait_for_recordings(Duration::from_secs(5)));
        assert!(started.elapsed() < Duration::from_secs(5));
        finisher.join().unwrap();
    }
}
