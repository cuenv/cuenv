//! Tests for the `CueEvaluator` builder pattern

use cuengine::{CueEvaluator, RetryConfig};
use std::fs;
use std::time::Duration;
use tempfile::TempDir;

#[test]
fn test_builder_with_cache() {
    let temp_dir = TempDir::new().unwrap();
    fs::write(
        temp_dir.path().join("test.cue"),
        "package test\nenv: {CACHED: \"value\"}",
    )
    .unwrap();

    let evaluator = CueEvaluator::builder()
        .cache_capacity(10)
        .cache_ttl(Duration::from_secs(60))
        .build()
        .unwrap();

    // First call - cache miss
    let result1 = evaluator.evaluate(temp_dir.path(), "test").unwrap();

    // Second call - should be cached
    let result2 = evaluator.evaluate(temp_dir.path(), "test").unwrap();

    assert_eq!(result1, result2);
}

#[test]
fn test_builder_with_validation() {
    let evaluator = CueEvaluator::builder()
        .max_package_name_length(10)
        .build()
        .unwrap();

    let temp_dir = TempDir::new().unwrap();

    // Should fail with long package name
    let result = evaluator.evaluate(temp_dir.path(), "this_is_a_very_long_package_name");
    assert!(result.is_err());

    // Should succeed with short name
    let _result = evaluator.evaluate(temp_dir.path(), "short");
    // Will fail due to missing CUE files, but validation passes
}

#[test]
fn test_builder_no_cache() {
    let evaluator = CueEvaluator::builder().cache_capacity(0).build().unwrap();

    // Should work without cache
    let temp_dir = TempDir::new().unwrap();
    fs::write(temp_dir.path().join("test.cue"), "package test\nenv: {}").unwrap();

    let _ = evaluator.evaluate(temp_dir.path(), "test");
}

#[test]
fn test_builder_with_custom_limits() {
    let evaluator = CueEvaluator::builder()
        .max_path_length(500)
        .max_package_name_length(50)
        .max_output_size(1024 * 1024)
        .build()
        .unwrap();

    let temp_dir = TempDir::new().unwrap();

    // Test with package name at limit
    let long_package = "a".repeat(49);
    let result = evaluator.evaluate(temp_dir.path(), &long_package);
    // Will fail due to missing CUE files or FFI, validation should pass
    // Just check that it doesn't fail with validation error
    if let Err(err) = result {
        let err_msg = err.to_string();
        assert!(!err_msg.contains("exceeds maximum length"));
    }

    // Test with package name over limit
    let too_long_package = "a".repeat(51);
    let result = evaluator.evaluate(temp_dir.path(), &too_long_package);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("exceeds maximum length")
    );
}

#[test]
fn test_builder_with_retry_config() {
    let retry_config = RetryConfig {
        max_attempts: 5,
        initial_delay: Duration::from_millis(200),
        max_delay: Duration::from_secs(5),
        exponential_base: 3.0,
    };

    let evaluator = CueEvaluator::builder()
        .retry_config(retry_config)
        .build()
        .unwrap();

    // The evaluator should be built with custom retry config
    let temp_dir = TempDir::new().unwrap();
    let _ = evaluator.evaluate(temp_dir.path(), "test");
}

#[test]
fn test_builder_no_retry() {
    let evaluator = CueEvaluator::builder().no_retry().build().unwrap();

    // The evaluator should be built without retry
    let temp_dir = TempDir::new().unwrap();
    let _ = evaluator.evaluate(temp_dir.path(), "test");
}

#[test]
fn test_builder_clear_cache() {
    let evaluator = CueEvaluator::builder().cache_capacity(10).build().unwrap();

    let temp_dir = TempDir::new().unwrap();
    fs::write(
        temp_dir.path().join("test.cue"),
        "package test\nenv: {DATA: \"value\"}",
    )
    .unwrap();

    // Make a call to populate cache
    let _ = evaluator.evaluate(temp_dir.path(), "test");

    // Clear the cache
    evaluator.clear_cache();

    // Cache should be empty, but evaluation should still work
    let _ = evaluator.evaluate(temp_dir.path(), "test");
}

#[test]
fn test_builder_default_values() {
    let builder = CueEvaluator::builder();
    let evaluator = builder.build().unwrap();

    // Default builder should create a valid evaluator
    let temp_dir = TempDir::new().unwrap();
    let _ = evaluator.evaluate(temp_dir.path(), "test");
}

#[test]
fn test_builder_cache_ttl() {
    let evaluator = CueEvaluator::builder()
        .cache_capacity(10)
        .cache_ttl(Duration::from_millis(100))
        .build()
        .unwrap();

    let temp_dir = TempDir::new().unwrap();
    fs::write(
        temp_dir.path().join("test.cue"),
        "package test\nenv: {TTL_TEST: \"value\"}",
    )
    .unwrap();

    // First call
    let _ = evaluator.evaluate(temp_dir.path(), "test");

    // Wait for TTL to expire
    std::thread::sleep(Duration::from_millis(150));

    // Should fetch again after TTL
    let _ = evaluator.evaluate(temp_dir.path(), "test");
}

#[test]
fn test_builder_chaining() {
    // Test that all builder methods can be chained
    let evaluator = CueEvaluator::builder()
        .max_path_length(1000)
        .max_package_name_length(100)
        .max_output_size(10 * 1024 * 1024)
        .cache_capacity(50)
        .cache_ttl(Duration::from_secs(300))
        .retry_config(RetryConfig::default())
        .build()
        .unwrap();

    let temp_dir = TempDir::new().unwrap();
    let _ = evaluator.evaluate(temp_dir.path(), "test");
}
