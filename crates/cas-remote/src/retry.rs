//! Bounded exponential-backoff retries and circuit breaking for remote cache RPCs.
//!
//! A remote cache is an optimization: transient network blips (`UNAVAILABLE`,
//! `DEADLINE_EXCEEDED`, HTTP 429/502/503/504) should be retried briefly, while
//! a persistently dead endpoint should trip the circuit breaker so the rest of
//! the build degrades immediately to the local cache instead of timing out on
//! every single action.

use crate::error::{Error, Result};
use std::future::Future;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Retry policy for transient remote cache RPC failures.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Total attempts including the initial call (minimum 1).
    pub max_attempts: usize,
    /// Delay before the first retry; doubled on each subsequent attempt.
    pub base_delay: Duration,
    /// Upper bound on backoff delay between attempts.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
        }
    }
}

impl RetryPolicy {
    /// Backoff delay before retry `attempt` (1-indexed: 1 = first retry).
    #[must_use]
    pub fn delay_for_retry(&self, retry_index: usize) -> Duration {
        let shift = u32::try_from(retry_index.saturating_sub(1)).unwrap_or(u32::MAX);
        let factor = 1_u32.checked_shl(shift).unwrap_or(u32::MAX);
        self.base_delay.saturating_mul(factor).min(self.max_delay)
    }
}

/// Lightweight consecutive-failure circuit breaker shared across a client.
#[derive(Debug)]
pub struct CircuitBreaker {
    failure_threshold: u32,
    cooldown: Duration,
    consecutive_failures: AtomicU32,
    open_until: Mutex<Option<Instant>>,
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new(5, Duration::from_secs(30))
    }
}

impl CircuitBreaker {
    /// Create a circuit breaker that opens after `failure_threshold`
    /// consecutive transient failures and stays open for `cooldown`.
    #[must_use]
    pub fn new(failure_threshold: u32, cooldown: Duration) -> Self {
        Self {
            failure_threshold: failure_threshold.max(1),
            cooldown,
            consecutive_failures: AtomicU32::new(0),
            open_until: Mutex::new(None),
        }
    }

    /// Reject requests immediately while the circuit is open and within cooldown.
    ///
    /// # Errors
    ///
    /// Returns an error if the circuit is currently open.
    pub fn check(&self, operation: &'static str) -> Result<()> {
        if let Ok(mut guard) = self.open_until.lock()
            && let Some(until) = *guard
        {
            if Instant::now() < until {
                return Err(Error::Rpc {
                    operation,
                    message: "remote cache circuit breaker is open after repeated failures"
                        .to_string(),
                });
            }
            // Cooldown elapsed: allow a half-open probe.
            *guard = None;
        }
        Ok(())
    }

    /// Record a successful RPC, closing the circuit.
    pub fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
        if let Ok(mut guard) = self.open_until.lock() {
            *guard = None;
        }
    }

    /// Record a transient transport failure; opens the circuit at threshold.
    pub fn record_failure(&self) {
        let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if failures >= self.failure_threshold
            && let Ok(mut guard) = self.open_until.lock()
        {
            *guard = Some(Instant::now() + self.cooldown);
        }
    }
}

/// Whether a gRPC status represents a transient failure worth retrying.
#[must_use]
pub fn is_transient_grpc(status: &tonic::Status) -> bool {
    matches!(
        status.code(),
        tonic::Code::Unavailable
            | tonic::Code::DeadlineExceeded
            | tonic::Code::ResourceExhausted
            | tonic::Code::Aborted
    )
}

/// Whether an HTTP status represents a transient failure worth retrying.
#[must_use]
pub fn is_transient_http_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502 | 503 | 504)
}

/// Runs async RPCs with a [`RetryPolicy`] and shared [`CircuitBreaker`].
#[derive(Clone, Copy)]
pub struct RetryRunner<'a> {
    /// Retry attempt count and backoff bounds.
    pub policy: RetryPolicy,
    /// Shared circuit breaker guarding the remote endpoint.
    pub breaker: &'a CircuitBreaker,
}

impl RetryRunner<'_> {
    /// Execute an async operation with bounded exponential backoff and circuit breaking.
    ///
    /// The closure returns `Ok(value)` on success or `Err((error, retryable))`.
    ///
    /// # Errors
    ///
    /// Returns the final error once retries are exhausted, the error is non-retryable,
    /// or the circuit breaker is open.
    pub async fn run<T, F, Fut>(&self, operation: &'static str, mut attempt_fn: F) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = std::result::Result<T, (Error, bool)>>,
    {
        self.breaker.check(operation)?;
        let attempts = self.policy.max_attempts.max(1);
        for attempt in 1..=attempts {
            match attempt_fn().await {
                Ok(value) => {
                    self.breaker.record_success();
                    return Ok(value);
                }
                Err((error, retryable)) => {
                    if retryable {
                        self.breaker.record_failure();
                        if attempt < attempts {
                            tokio::time::sleep(self.policy.delay_for_retry(attempt)).await;
                            continue;
                        }
                    }
                    return Err(error);
                }
            }
        }
        unreachable!("attempts is always >= 1")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn backoff_doubles_and_caps_at_max_delay() {
        let policy = RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(350),
        };
        assert_eq!(policy.delay_for_retry(1), Duration::from_millis(100));
        assert_eq!(policy.delay_for_retry(2), Duration::from_millis(200));
        assert_eq!(policy.delay_for_retry(3), Duration::from_millis(350));
        assert_eq!(policy.delay_for_retry(4), Duration::from_millis(350));
    }

    #[tokio::test]
    async fn retries_transient_errors_and_succeeds() {
        let breaker = CircuitBreaker::default();
        let runner = RetryRunner {
            policy: RetryPolicy {
                max_attempts: 3,
                base_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(5),
            },
            breaker: &breaker,
        };
        let calls = AtomicUsize::new(0);
        let value = runner
            .run("TestOp", || {
                let n = calls.fetch_add(1, Ordering::Relaxed) + 1;
                async move {
                    if n < 3 {
                        Err((
                            Error::Rpc {
                                operation: "TestOp",
                                message: "transient".into(),
                            },
                            true,
                        ))
                    } else {
                        Ok(42)
                    }
                }
            })
            .await
            .unwrap();
        assert_eq!(value, 42);
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn does_not_retry_non_transient_errors() {
        let breaker = CircuitBreaker::default();
        let runner = RetryRunner {
            policy: RetryPolicy {
                max_attempts: 3,
                base_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(5),
            },
            breaker: &breaker,
        };
        let calls = AtomicUsize::new(0);
        let err = runner
            .run::<(), _, _>("TestOp", || {
                calls.fetch_add(1, Ordering::Relaxed);
                async {
                    Err((
                        Error::Rpc {
                            operation: "TestOp",
                            message: "permission denied".into(),
                        },
                        false,
                    ))
                }
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("permission denied"));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn circuit_breaker_opens_at_threshold_and_closes_on_success() {
        let breaker = CircuitBreaker::new(2, Duration::from_secs(60));
        assert!(breaker.check("Op").is_ok());
        breaker.record_failure();
        assert!(breaker.check("Op").is_ok());
        breaker.record_failure();
        assert!(breaker.check("Op").is_err());
        breaker.record_success();
        assert!(breaker.check("Op").is_ok());
    }
}
