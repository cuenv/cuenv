use std::time::Duration;
use tokio::time::sleep;
use crate::RemoteError;

pub struct RetryConfig {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub backoff_multiplier: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(10),
            backoff_multiplier: 2.0,
        }
    }
}

pub async fn retry<F, Fut, T>(config: &RetryConfig, mut operation: F) -> Result<T, RemoteError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, RemoteError>>,
{
    let mut attempt = 0;
    let mut backoff = config.initial_backoff;

    loop {
        attempt += 1;
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                if attempt >= config.max_attempts {
                    return Err(e);
                }

                if is_retryable(&e) {
                    sleep(backoff).await;
                    backoff = std::cmp::min(
                        backoff.mul_f64(config.backoff_multiplier),
                        config.max_backoff,
                    );
                } else {
                    return Err(e);
                }
            }
        }
    }
}

fn is_retryable(error: &RemoteError) -> bool {
    match error {
        RemoteError::Grpc(status) => match status.code() {
            tonic::Code::Unavailable | tonic::Code::ResourceExhausted | tonic::Code::DeadlineExceeded => true,
            _ => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_retry_success() {
        let config = RetryConfig::default();
        let result = retry(&config, || async { Ok::<_, RemoteError>(42) }).await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_retry_fail_recover() {
        let config = RetryConfig::default();
        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_clone = attempts.clone();

        let result = retry(&config, || {
            let attempts = attempts_clone.clone();
            async move {
                let count = attempts.fetch_add(1, Ordering::SeqCst);
                if count < 1 {
                    Err(RemoteError::Grpc(tonic::Status::new(tonic::Code::Unavailable, "fail")))
                } else {
                    Ok(42)
                }
            }
        }).await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_retry_fail_persistent() {
        let config = RetryConfig {
            max_attempts: 2,
            ..Default::default()
        };
        let result = retry(&config, || async {
            Err::<(), _>(RemoteError::Grpc(tonic::Status::new(tonic::Code::Unavailable, "fail")))
        }).await;

        assert!(result.is_err());
    }
}
