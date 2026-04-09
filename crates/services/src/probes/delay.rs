//! Simple delay readiness probe.
//!
//! Waits a fixed duration before reporting ready. Escape hatch only.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::{ProbeOutcome, ProbeRunner};

/// Probes readiness by waiting a fixed duration.
pub struct DelayProbe {
    delay: Duration,
    ready: Arc<AtomicBool>,
}

impl DelayProbe {
    /// Create a new delay probe.
    #[must_use]
    pub fn new(delay: Duration) -> Self {
        let ready = Arc::new(AtomicBool::new(false));
        let ready_clone = Arc::clone(&ready);
        let delay_clone = delay;

        // Spawn a background task that sets ready after the delay
        tokio::spawn(async move {
            tokio::time::sleep(delay_clone).await;
            ready_clone.store(true, Ordering::Release);
        });

        Self { delay, ready }
    }

    /// Get the configured delay.
    #[must_use]
    pub fn delay(&self) -> Duration {
        self.delay
    }
}

#[async_trait]
impl ProbeRunner for DelayProbe {
    async fn check(&self) -> ProbeOutcome {
        if self.ready.load(Ordering::Acquire) {
            ProbeOutcome::Ready
        } else {
            ProbeOutcome::NotReady
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_delay_probe_not_ready_immediately() {
        let probe = DelayProbe::new(Duration::from_secs(60));
        let result = probe.check().await;
        assert!(matches!(result, ProbeOutcome::NotReady));
    }

    #[tokio::test]
    async fn test_delay_probe_ready_after_delay() {
        let probe = DelayProbe::new(Duration::from_millis(50));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let result = probe.check().await;
        assert!(matches!(result, ProbeOutcome::Ready));
    }
}
