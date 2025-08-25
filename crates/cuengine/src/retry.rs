//! Retry logic with exponential backoff

use cuenv_core::Result;
use std::time::Duration;
use std::thread;

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
                    attempt, config.max_attempts, e, delay
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