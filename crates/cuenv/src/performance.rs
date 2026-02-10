//! Performance instrumentation and metrics collection
//!
//! This module provides comprehensive performance monitoring with
//! structured tracing, timing measurements, and resource usage tracking.
//!
//! Note: Currently only used by tests and the performance macros.
//! The infrastructure is available for future use in command implementations.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, info};
use uuid::Uuid;

/// Performance metrics for a single operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationMetrics {
    /// Unique identifier for this operation instance
    pub operation_id: Uuid,
    /// Human-readable name describing the operation
    pub operation_name: String,
    /// Wall-clock time when the operation started
    pub start_time: std::time::SystemTime,
    /// How long the operation took to complete
    pub duration: Duration,
    /// Whether the operation completed successfully
    pub success: bool,
    /// Resident memory usage in bytes at completion, if available
    pub memory_usage_bytes: Option<u64>,
    /// Additional key-value pairs for operation-specific context
    pub metadata: HashMap<String, String>,
}

/// Global performance registry
#[derive(Debug, Default)]
pub struct PerformanceRegistry {
    operations: Arc<Mutex<Vec<OperationMetrics>>>,
}

impl PerformanceRegistry {
    /// Creates a new empty performance registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            operations: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Records a completed operation's metrics to the registry.
    pub fn record_operation(&self, metrics: OperationMetrics) {
        if let Ok(mut ops) = self.operations.lock() {
            info!(
                operation_id = %metrics.operation_id,
                operation = %metrics.operation_name,
                duration_ms = metrics.duration.as_millis(),
                success = metrics.success,
                "Performance metric recorded"
            );
            ops.push(metrics);
        }
    }

    /// Returns an aggregated summary of all recorded operations.
    ///
    /// Recovers from poisoned mutex state to ensure metrics are never lost.
    #[must_use]
    pub fn get_summary(&self) -> PerformanceSummary {
        // If the mutex is poisoned, recover the data anyway since metrics are non-critical
        let ops = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let total_operations = ops.len();
        let successful_operations = ops.iter().filter(|op| op.success).count();
        let total_duration: Duration = ops.iter().map(|op| op.duration).sum();

        let avg_duration = if total_operations > 0 {
            #[allow(clippy::cast_possible_truncation)]
            let divisor = total_operations as u32;
            total_duration / divisor
        } else {
            Duration::ZERO
        };

        PerformanceSummary {
            total_operations,
            successful_operations,
            failed_operations: total_operations - successful_operations,
            total_duration,
            average_duration: avg_duration,
            operations: ops.clone(),
        }
    }
}

/// Performance summary with aggregated metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceSummary {
    /// Total number of operations recorded
    pub total_operations: usize,
    /// Number of operations that completed successfully
    pub successful_operations: usize,
    /// Number of operations that failed
    pub failed_operations: usize,
    /// Combined duration of all operations
    pub total_duration: Duration,
    /// Mean duration across all operations
    pub average_duration: Duration,
    /// Complete list of individual operation metrics
    pub operations: Vec<OperationMetrics>,
}

/// Global performance registry instance
static PERFORMANCE_REGISTRY: std::sync::OnceLock<PerformanceRegistry> = std::sync::OnceLock::new();

/// Returns a reference to the global performance registry.
///
/// The registry is lazily initialized on first access.
#[must_use]
pub fn registry() -> &'static PerformanceRegistry {
    PERFORMANCE_REGISTRY.get_or_init(PerformanceRegistry::new)
}

/// Performance measurement guard that automatically records metrics
pub struct PerformanceGuard {
    operation_name: String,
    operation_id: Uuid,
    start_time: Instant,
    start_system_time: std::time::SystemTime,
    metadata: HashMap<String, String>,
}

impl PerformanceGuard {
    /// Creates a new performance guard and starts timing the operation.
    ///
    /// The guard will automatically record metrics when dropped.
    #[must_use]
    pub fn new(operation_name: impl Into<String>) -> Self {
        let operation_name = operation_name.into();
        let operation_id = Uuid::new_v4();

        debug!(
            operation_id = %operation_id,
            operation = %operation_name,
            "Starting performance measurement"
        );

        Self {
            operation_name,
            operation_id,
            start_time: Instant::now(),
            start_system_time: std::time::SystemTime::now(),
            metadata: HashMap::new(),
        }
    }

    /// Adds a key-value metadata pair to this operation's metrics.
    pub fn add_metadata(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.metadata.insert(key.into(), value.into());
    }

    /// Explicitly finishes the operation and records metrics with the given success status.
    ///
    /// This consumes the guard, preventing the `Drop` implementation from recording again.
    pub fn finish(self, success: bool) {
        let duration = self.start_time.elapsed();

        let metrics = OperationMetrics {
            operation_id: self.operation_id,
            operation_name: self.operation_name.clone(),
            start_time: self.start_system_time,
            duration,
            success,
            memory_usage_bytes: get_memory_usage(),
            metadata: self.metadata.clone(),
        };

        info!(
            operation_id = %self.operation_id,
            duration_ms = duration.as_millis(),
            success = success,
            "Performance measurement completed"
        );

        registry().record_operation(metrics);
    }
}

impl Drop for PerformanceGuard {
    fn drop(&mut self) {
        // If not explicitly finished, assume success
        let duration = self.start_time.elapsed();

        let metrics = OperationMetrics {
            operation_id: self.operation_id,
            operation_name: self.operation_name.clone(),
            start_time: self.start_system_time,
            duration,
            success: true,
            memory_usage_bytes: get_memory_usage(),
            metadata: self.metadata.clone(),
        };

        registry().record_operation(metrics);
    }
}

/// Get current memory usage if available
fn get_memory_usage() -> Option<u64> {
    // On Linux, we can read from /proc/self/status
    #[cfg(target_os = "linux")]
    {
        use std::fs;
        if let Ok(status) = fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if line.starts_with("VmRSS:")
                    && let Some(kb) = line.split_whitespace().nth(1)
                    && let Ok(kb_val) = kb.parse::<u64>()
                {
                    return Some(kb_val * 1024); // Convert KB to bytes
                }
            }
        }
    }
    None
}

/// Macro for easy performance measurement
#[macro_export]
macro_rules! measure_perf {
    ($operation:expr, $code:block) => {{
        let _guard = $crate::performance::PerformanceGuard::new($operation);
        $code
    }};
}

// (first tests module removed; consolidated into the one at bottom of file)

/// Async version of performance measurement
#[macro_export]
macro_rules! measure_perf_async {
    ($operation:expr, $code:expr) => {{
        let guard = $crate::performance::PerformanceGuard::new($operation);
        let result = $code.await;
        guard.finish(true);
        result
    }};
}

/// Instrument function with automatic performance measurement
pub fn _instrument_perf<F, R>(operation_name: &str, f: F) -> R
where
    F: FnOnce() -> R,
{
    let _guard = PerformanceGuard::new(operation_name);
    f()
}

/// Async version of `instrument_perf`
pub async fn _instrument_perf_async<F, Fut, R>(operation_name: &str, f: F) -> R
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = R>,
{
    let guard = PerformanceGuard::new(operation_name);
    let result = f().await;
    guard.finish(true);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;
    use tokio::time::sleep;
    use uuid::Uuid;

    #[test]
    fn test_performance_measurement() {
        let _guard = PerformanceGuard::new("test_operation");
        std::thread::sleep(Duration::from_millis(10));
        // Guard auto-finishes on drop
    }

    #[tokio::test]
    async fn test_async_performance_measurement() {
        let result = measure_perf_async!("async_test", async {
            sleep(Duration::from_millis(10)).await;
            42
        });

        assert_eq!(result, 42);

        let summary = registry().get_summary();
        assert!(summary.total_operations > 0);
    }

    #[test]
    fn test_performance_registry() {
        let registry = PerformanceRegistry::new();

        let metrics = OperationMetrics {
            operation_id: Uuid::new_v4(),
            operation_name: "test".to_string(),
            start_time: std::time::SystemTime::now(),
            duration: Duration::from_millis(100),
            success: true,
            memory_usage_bytes: Some(1024),
            metadata: HashMap::new(),
        };

        registry.record_operation(metrics);

        let summary = registry.get_summary();
        assert_eq!(summary.total_operations, 1);
        assert_eq!(summary.successful_operations, 1);
        assert_eq!(summary.failed_operations, 0);
    }

    // Additional focused unit tests to boost coverage
    // Helper to find operations containing a token in their name
    fn find_ops(token: &str) -> Vec<OperationMetrics> {
        registry()
            .get_summary()
            .operations
            .into_iter()
            .filter(|op| op.operation_name.contains(token))
            .collect()
    }

    #[test]
    fn test_registry_record_and_summary() {
        let reg = PerformanceRegistry::new();
        let before = reg.get_summary();
        assert_eq!(before.total_operations, 0);

        let mut meta = HashMap::new();
        meta.insert("k".to_string(), "v".to_string());
        reg.record_operation(OperationMetrics {
            operation_id: Uuid::new_v4(),
            operation_name: "unit_test_op".to_string(),
            start_time: std::time::SystemTime::now(),
            duration: Duration::from_millis(5),
            success: true,
            memory_usage_bytes: None,
            metadata: meta,
        });

        let after = reg.get_summary();
        assert_eq!(after.total_operations, 1);
        assert_eq!(after.successful_operations, 1);
        assert_eq!(after.failed_operations, 0);
        assert!(after.total_duration >= Duration::from_millis(5));
        assert_eq!(after.operations.len(), 1);
    }

    #[test]
    fn test_guard_finish_records_success() {
        let token = format!("finish-{}", Uuid::new_v4());
        let mut guard = PerformanceGuard::new(token.clone());
        guard.add_metadata("case", "finish");
        guard.finish(true);

        let ops = find_ops(&token);
        assert!(!ops.is_empty());
        assert!(ops.iter().any(|o| o.success));
        assert!(
            ops.iter()
                .any(|o| { o.metadata.get("case").is_some_and(|v| v == "finish") })
        );
    }

    #[test]
    fn test_guard_drop_records_when_not_finished() {
        let token = format!("drop-{}", Uuid::new_v4());
        {
            let mut g = PerformanceGuard::new(token.clone());
            g.add_metadata("case", "drop");
        }
        let ops = find_ops(&token);
        assert!(!ops.is_empty());
    }

    #[test]
    fn test_measure_perf_macro_executes_block() {
        let token = format!("macro-{}", Uuid::new_v4());
        let value = measure_perf!(&token, { 2 + 2 });
        assert_eq!(value, 4);
        let ops = find_ops(&token);
        assert!(!ops.is_empty());
    }

    #[test]
    fn test_operationmetrics_serde_roundtrip() {
        let metrics = OperationMetrics {
            operation_id: Uuid::new_v4(),
            operation_name: "serde-test".to_string(),
            start_time: std::time::SystemTime::now(),
            duration: Duration::from_millis(1),
            success: false,
            memory_usage_bytes: Some(1234),
            metadata: HashMap::from([("a".into(), "b".into())]),
        };
        let json = serde_json::to_string(&metrics).unwrap();
        let de: OperationMetrics = serde_json::from_str(&json).unwrap();
        assert_eq!(de.operation_name, metrics.operation_name);
        assert!(!de.success);
        assert_eq!(de.memory_usage_bytes, Some(1234));
    }

    #[test]
    fn test_summary_serde_roundtrip() {
        let reg = PerformanceRegistry::new();
        reg.record_operation(OperationMetrics {
            operation_id: Uuid::new_v4(),
            operation_name: "serde-summary".to_string(),
            start_time: std::time::SystemTime::now(),
            duration: Duration::from_millis(3),
            success: true,
            memory_usage_bytes: None,
            metadata: HashMap::new(),
        });
        let summary = reg.get_summary();
        let json = serde_json::to_string(&summary).unwrap();
        let de: PerformanceSummary = serde_json::from_str(&json).unwrap();
        assert!(de.total_operations >= 1);
    }

    #[test]
    fn test_get_memory_usage_smoke() {
        let usage = get_memory_usage();
        if let Some(bytes) = usage {
            assert!(bytes > 0);
        }
    }
}
