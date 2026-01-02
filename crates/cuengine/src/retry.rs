//! Retry logic with exponential backoff

use crate::error::Result;
use std::thread;
use std::time::Duration;

/// Configuration for retry behavior
pub struct RetryConfig {
    /// Maximum number of retry attempts
    pub max_attempts: u32,
    /// Initial delay before first retry
    pub initial_delay: Duration,
    /// Maximum delay between retries
    pub max_delay: Duration,
    /// Base for exponential backoff calculation
    pub exponential_base: f32,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
            exponential_base: 2.0,
        }
    }
}

/// Execute an operation with retry logic
///
/// # Errors
///
/// Returns the last error if all retry attempts fail
pub fn with_retry<T, F>(config: &RetryConfig, mut operation: F) -> Result<T>
where
    F: FnMut() -> Result<T>,
{
    let mut attempt = 0;
    let mut delay = config.initial_delay;

    loop {
        attempt += 1;

        match operation() {
            Ok(result) => return Ok(result),
            Err(e) if attempt >= config.max_attempts => return Err(e),
            Err(e) => {
                // Log the retry attempt
                tracing::warn!(
                    "Operation failed (attempt {}/{}): {}. Retrying in {:?}",
                    attempt,
                    config.max_attempts,
                    e,
                    delay
                );

                // Sleep before retry
                thread::sleep(delay);

                // Calculate next delay with exponential backoff
                let next_delay = delay.mul_f32(config.exponential_base);
                delay = next_delay.min(config.max_delay);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CueEngineError;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[test]
    fn test_retry_config_default() {
        let config = RetryConfig::default();
        assert_eq!(config.max_attempts, 3);
        assert_eq!(config.initial_delay, Duration::from_millis(100));
        assert_eq!(config.max_delay, Duration::from_secs(10));
        assert!((config.exponential_base - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_with_retry_success_first_attempt() {
        let config = RetryConfig::default();
        let result = with_retry(&config, || Ok::<i32, CueEngineError>(42));
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn test_with_retry_all_failures() {
        let config = RetryConfig {
            max_attempts: 2,
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
            exponential_base: 2.0,
        };

        let result: Result<i32> = with_retry(&config, || {
            Err(CueEngineError::validation("always fails"))
        });

        assert!(result.is_err());
    }

    #[test]
    fn test_with_retry_eventual_success() {
        let config = RetryConfig {
            max_attempts: 3,
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
            exponential_base: 2.0,
        };

        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_clone = Arc::clone(&attempts);

        let result = with_retry(&config, move || {
            let current = attempts_clone.fetch_add(1, Ordering::SeqCst);
            if current < 2 {
                Err(CueEngineError::validation("fail first two times"))
            } else {
                Ok(42)
            }
        });

        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn test_with_retry_respects_max_attempts() {
        let config = RetryConfig {
            max_attempts: 2,
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
            exponential_base: 2.0,
        };

        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_clone = Arc::clone(&attempts);

        let result: Result<i32> = with_retry(&config, move || {
            attempts_clone.fetch_add(1, Ordering::SeqCst);
            Err(CueEngineError::validation("always fails"))
        });

        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}
