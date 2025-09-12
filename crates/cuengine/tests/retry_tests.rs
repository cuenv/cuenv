//! Tests for retry logic with exponential backoff

use cuengine::retry::{RetryConfig, with_retry};
use cuenv_core::Error;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[test]
fn test_retry_success_first_attempt() {
    let config = RetryConfig::default();
    let mut attempt_count = 0;

    let result = with_retry(&config, || {
        attempt_count += 1;
        Ok::<String, Error>("success".to_string())
    });

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "success");
    assert_eq!(attempt_count, 1); // Should succeed on first attempt
}

#[test]
fn test_retry_eventual_success() {
    let config = RetryConfig {
        max_attempts: 3,
        initial_delay: Duration::from_millis(10),
        max_delay: Duration::from_secs(1),
        exponential_base: 2.0,
    };

    let attempt_count = Arc::new(Mutex::new(0));
    let attempt_count_clone = attempt_count.clone();

    let result = with_retry(&config, || {
        let mut count = attempt_count_clone.lock().unwrap();
        *count += 1;

        if *count < 3 {
            Err(Error::configuration("temporary failure"))
        } else {
            Ok("success after retries".to_string())
        }
    });

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "success after retries");
    assert_eq!(*attempt_count.lock().unwrap(), 3);
}

#[test]
fn test_retry_max_attempts_exceeded() {
    let config = RetryConfig {
        max_attempts: 2,
        initial_delay: Duration::from_millis(10),
        max_delay: Duration::from_secs(1),
        exponential_base: 2.0,
    };

    let attempt_count = Arc::new(Mutex::new(0));
    let attempt_count_clone = attempt_count.clone();

    let result = with_retry(&config, || {
        let mut count = attempt_count_clone.lock().unwrap();
        *count += 1;
        Err::<String, Error>(Error::configuration("persistent failure"))
    });

    assert!(result.is_err());
    assert_eq!(*attempt_count.lock().unwrap(), 2); // Should stop after max_attempts
}

#[test]
fn test_retry_exponential_backoff() {
    let config = RetryConfig {
        max_attempts: 4,
        initial_delay: Duration::from_millis(50),
        max_delay: Duration::from_millis(500),
        exponential_base: 2.0,
    };

    let attempt_times = Arc::new(Mutex::new(Vec::new()));
    let attempt_times_clone = attempt_times.clone();

    let start = Instant::now();

    let _ = with_retry(&config, || {
        let mut times = attempt_times_clone.lock().unwrap();
        times.push(start.elapsed());
        Err::<String, Error>(Error::configuration("failure"))
    });

    let times = attempt_times.lock().unwrap();
    assert_eq!(times.len(), 4);

    // Verify delays are increasing (with some tolerance for timing)
    // First attempt should be immediate
    assert!(times[0] < Duration::from_millis(10));

    // Subsequent attempts should have delays
    // Note: We can't be too precise due to thread scheduling
    if times.len() > 1 {
        assert!(times[1] >= Duration::from_millis(40)); // ~50ms delay
    }
    if times.len() > 2 {
        assert!(times[2] >= Duration::from_millis(90)); // ~50ms + 100ms
    }
    if times.len() > 3 {
        assert!(times[3] >= Duration::from_millis(190)); // ~50ms + 100ms + 200ms
    }
}

#[test]
fn test_retry_max_delay_capping() {
    let config = RetryConfig {
        max_attempts: 5,
        initial_delay: Duration::from_millis(100),
        max_delay: Duration::from_millis(150), // Cap at 150ms
        exponential_base: 10.0,                // High base to test capping
    };

    let attempt_times = Arc::new(Mutex::new(Vec::new()));
    let attempt_times_clone = attempt_times.clone();

    let start = Instant::now();

    let _ = with_retry(&config, || {
        let mut times = attempt_times_clone.lock().unwrap();
        times.push(start.elapsed());
        Err::<String, Error>(Error::configuration("failure"))
    });

    let times = attempt_times.lock().unwrap();

    // After the second attempt, delays should be capped at max_delay
    // The total time for attempts 3-5 should reflect the capped delay
    if times.len() >= 5 {
        // Calculate delay between attempt 3 and 4
        let delay_3_to_4 = times[3] - times[2];
        // Should be around 150ms (max_delay), but allow more tolerance for OS scheduling
        assert!(delay_3_to_4 >= Duration::from_millis(100)); // Should be at least close to max_delay
        assert!(delay_3_to_4 <= Duration::from_millis(400)); // More generous upper bound

        // Calculate delay between attempt 4 and 5
        let delay_4_to_5 = times[4] - times[3];
        // Should also be around 150ms (max_delay), but allow more tolerance for OS scheduling
        assert!(delay_4_to_5 >= Duration::from_millis(100)); // Should be at least close to max_delay
        assert!(delay_4_to_5 <= Duration::from_millis(400)); // More generous upper bound
    }
}

#[test]
fn test_retry_config_default() {
    let config = RetryConfig::default();

    assert_eq!(config.max_attempts, 3);
    assert_eq!(config.initial_delay, Duration::from_millis(100));
    assert_eq!(config.max_delay, Duration::from_secs(10));
    assert!((config.exponential_base - 2.0).abs() < f32::EPSILON);
}

#[test]
fn test_retry_with_different_error_types() {
    let config = RetryConfig {
        max_attempts: 2,
        initial_delay: Duration::from_millis(10),
        max_delay: Duration::from_secs(1),
        exponential_base: 2.0,
    };

    // Test with Validation error
    let result = with_retry(&config, || {
        Err::<String, Error>(Error::validation("validation error"))
    });
    assert!(result.is_err());

    // Test with Timeout error
    let result = with_retry(&config, || {
        Err::<String, Error>(Error::Timeout { seconds: 5 })
    });
    assert!(result.is_err());
}

#[test]
fn test_retry_immediate_success_no_delay() {
    let config = RetryConfig {
        max_attempts: 3,
        initial_delay: Duration::from_secs(10), // Long delay that shouldn't be used
        max_delay: Duration::from_secs(100),
        exponential_base: 2.0,
    };

    let start = Instant::now();

    let result = with_retry(&config, || {
        Ok::<String, Error>("immediate success".to_string())
    });

    let elapsed = start.elapsed();

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "immediate success");
    // Should complete quickly without any delays
    assert!(elapsed < Duration::from_millis(100));
}
