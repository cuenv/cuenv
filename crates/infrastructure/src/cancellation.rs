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
//!    provider process synchronously, so the command can exit at once.
//!
//! Providers run in their own process group, so a Ctrl-C typed at the
//! terminal reaches cuenv only and cuenv decides what the providers see.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

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

    /// Second interrupt: kill every provider process now.
    ///
    /// Synchronous and safe to call from any thread, including a signal
    /// handling path that is about to exit the process. Providers launched
    /// afterwards are killed as soon as they start. Also implies
    /// [`Cancellation::stop`]'s flag, so no new resource is started.
    pub fn terminate_providers(&self) {
        self.shared.stop_requested.store(true, Ordering::SeqCst);
        self.shared.terminated.store(true, Ordering::SeqCst);
        for provider in self.live_providers() {
            provider.kill();
        }
    }

    /// Whether [`Cancellation::terminate_providers`] was called.
    #[must_use]
    pub fn is_terminated(&self) -> bool {
        self.shared.terminated.load(Ordering::SeqCst)
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
}
