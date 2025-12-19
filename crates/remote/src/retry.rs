//! Retry logic with exponential backoff for REAPI operations

use crate::config::RetryConfig;
use crate::error::{RemoteError, Result};
use backoff::{backoff::Backoff, ExponentialBackoff, ExponentialBackoffBuilder};
use std::time::Duration;
use tracing::{debug, warn};

/// Retry a fallible async operation with exponential backoff
pub async fn retry_with_backoff<F, Fut, T>(
    config: &RetryConfig,
    operation_name: &str,
    mut f: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut backoff = create_backoff(config);
    let mut attempts = 0;

    loop {
        attempts += 1;

        match f().await {
            Ok(result) => {
                if attempts > 1 {
                    debug!(
                        operation = operation_name,
                        attempts = attempts,
                        "Operation succeeded after retry"
                    );
                }
                return Ok(result);
            }
            Err(err) => {
                if attempts >= config.max_attempts {
                    warn!(
                        operation = operation_name,
                        attempts = attempts,
                        error = %err,
                        "Operation failed after maximum retries"
                    );
                    return Err(RemoteError::retry_exhausted(operation_name, attempts));
                }

                // Check if error is retryable
                if !is_retryable(&err) {
                    debug!(
                        operation = operation_name,
                        error = %err,
                        "Error is not retryable, failing immediately"
                    );
                    return Err(err);
                }

                // Get next backoff duration
                if let Some(duration) = backoff.next_backoff() {
                    warn!(
                        operation = operation_name,
                        attempts = attempts,
                        error = %err,
                        retry_in_ms = duration.as_millis(),
                        "Operation failed, retrying"
                    );
                    tokio::time::sleep(duration).await;
                } else {
                    // Backoff exhausted
                    return Err(RemoteError::retry_exhausted(operation_name, attempts));
                }
            }
        }
    }
}

/// Create exponential backoff from config
fn create_backoff(config: &RetryConfig) -> ExponentialBackoff {
    ExponentialBackoffBuilder::new()
        .with_initial_interval(Duration::from_millis(config.initial_backoff_ms))
        .with_max_interval(Duration::from_millis(config.max_backoff_ms))
        .with_multiplier(config.backoff_multiplier)
        .with_max_elapsed_time(None) // We use max_attempts instead
        .build()
}

/// Determine if an error is retryable
fn is_retryable(err: &RemoteError) -> bool {
    match err {
        // Network/connection errors are retryable
        RemoteError::ConnectionFailed { .. } => true,

        // gRPC errors may be retryable depending on status code
        RemoteError::GrpcError { source, .. } => match source.code() {
            tonic::Code::Unavailable => true,
            tonic::Code::ResourceExhausted => true,
            tonic::Code::DeadlineExceeded => true,
            tonic::Code::Internal => true,
            tonic::Code::Unknown => true,
            _ => false,
        },

        // Timeouts are retryable
        RemoteError::Timeout { .. } => true,

        // I/O errors are retryable
        RemoteError::IoError { .. } => true,

        // These errors are NOT retryable
        RemoteError::ContentNotFound { .. } => false,
        RemoteError::InvalidDigest(_) => false,
        RemoteError::MerkleError { .. } => false,
        RemoteError::ExecutionFailed { .. } => false,
        RemoteError::AuthenticationFailed { .. } => false,
        RemoteError::ConfigError(_) => false,
        RemoteError::SerializationError { .. } => false,
        RemoteError::RetryExhausted { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_retry_success_first_attempt() {
        let config = RetryConfig::default();
        let mut call_count = 0;

        let result = retry_with_backoff(&config, "test", || async {
            call_count += 1;
            Ok::<_, RemoteError>(42)
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count, 1);
    }

    #[tokio::test]
    async fn test_retry_success_after_failure() {
        let config = RetryConfig {
            max_attempts: 3,
            initial_backoff_ms: 10,
            max_backoff_ms: 100,
            backoff_multiplier: 2.0,
        };
        let mut call_count = 0;

        let result = retry_with_backoff(&config, "test", || async {
            call_count += 1;
            if call_count < 3 {
                Err(RemoteError::timeout("test", 1))
            } else {
                Ok::<_, RemoteError>(42)
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count, 3);
    }

    #[tokio::test]
    async fn test_retry_exhausted() {
        let config = RetryConfig {
            max_attempts: 2,
            initial_backoff_ms: 10,
            max_backoff_ms: 100,
            backoff_multiplier: 2.0,
        };
        let mut call_count = 0;

        let result = retry_with_backoff(&config, "test", || async {
            call_count += 1;
            Err::<i32, _>(RemoteError::timeout("test", 1))
        })
        .await;

        assert!(result.is_err());
        assert_eq!(call_count, 2);
        assert!(matches!(result.unwrap_err(), RemoteError::RetryExhausted { .. }));
    }

    #[tokio::test]
    async fn test_non_retryable_error() {
        let config = RetryConfig::default();
        let mut call_count = 0;

        let result = retry_with_backoff(&config, "test", || async {
            call_count += 1;
            Err::<i32, _>(RemoteError::invalid_digest("bad digest"))
        })
        .await;

        assert!(result.is_err());
        assert_eq!(call_count, 1); // Should not retry
    }
}
