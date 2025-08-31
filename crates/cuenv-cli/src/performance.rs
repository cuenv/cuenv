//! Performance instrumentation and metrics collection
//!
//! This module provides comprehensive performance monitoring with
//! structured tracing, timing measurements, and resource usage tracking.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use tracing::{info, debug};
use uuid::Uuid;

/// Performance metrics for a single operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationMetrics {
    pub operation_id: Uuid,
    pub operation_name: String,
    pub start_time: std::time::SystemTime,
    pub duration: Duration,
    pub success: bool,
    pub memory_usage_bytes: Option<u64>,
    pub metadata: HashMap<String, String>,
}

/// Global performance registry
#[derive(Debug, Default)]
pub struct PerformanceRegistry {
    operations: Arc<Mutex<Vec<OperationMetrics>>>,
}

impl PerformanceRegistry {
    pub fn new() -> Self {
        Self {
            operations: Arc::new(Mutex::new(Vec::new())),
        }
    }

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

    pub fn get_summary(&self) -> PerformanceSummary {
        let ops = self.operations.lock().unwrap();
        let total_operations = ops.len();
        let successful_operations = ops.iter().filter(|op| op.success).count();
        let total_duration: Duration = ops.iter().map(|op| op.duration).sum();
        
        let avg_duration = if total_operations > 0 {
            total_duration / total_operations as u32
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
    pub total_operations: usize,
    pub successful_operations: usize,
    pub failed_operations: usize,
    pub total_duration: Duration,
    pub average_duration: Duration,
    pub operations: Vec<OperationMetrics>,
}

/// Global performance registry instance
static PERFORMANCE_REGISTRY: std::sync::OnceLock<PerformanceRegistry> = std::sync::OnceLock::new();

/// Get the global performance registry
pub fn registry() -> &'static PerformanceRegistry {
    PERFORMANCE_REGISTRY.get_or_init(|| PerformanceRegistry::new())
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

    pub fn add_metadata(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.metadata.insert(key.into(), value.into());
    }

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
                if line.starts_with("VmRSS:") {
                    if let Some(kb) = line.split_whitespace().nth(1) {
                        if let Ok(kb_val) = kb.parse::<u64>() {
                            return Some(kb_val * 1024); // Convert KB to bytes
                        }
                    }
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
pub fn instrument_perf<F, R>(operation_name: &str, f: F) -> R
where
    F: FnOnce() -> R,
{
    let _guard = PerformanceGuard::new(operation_name);
    f()
}

/// Async version of instrument_perf
pub async fn instrument_perf_async<F, Fut, R>(operation_name: &str, f: F) -> R
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
    use std::time::Duration;
    use tokio::time::sleep;

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
}