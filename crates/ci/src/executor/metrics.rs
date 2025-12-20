//! Cache Metrics
//!
//! Provides metrics collection for cache operations and task execution.
//! Metrics are exposed in formats compatible with Prometheus and OpenTelemetry.

use crate::ir::CachePolicy;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Cache metrics collector
#[derive(Debug, Default)]
pub struct CacheMetrics {
    /// Total cache hits by policy
    hits: PolicyCounters,
    /// Total cache misses by policy
    misses: PolicyCounters,
    /// Total cache restore failures by error type
    restore_failures: ErrorCounters,
    /// Total bytes downloaded from cache
    bytes_downloaded: AtomicU64,
    /// Total bytes uploaded to cache
    bytes_uploaded: AtomicU64,
    /// Total cache check latency (microseconds)
    check_latency_us: AtomicU64,
    /// Total cache checks
    check_count: AtomicU64,
    /// Task execution durations (microseconds)
    task_durations: TaskDurations,
    /// Runtime materialization durations (microseconds)
    runtime_durations: RuntimeDurations,
}

/// Task execution duration tracking
#[derive(Debug, Default)]
struct TaskDurations {
    /// Total execution time across all tasks (microseconds)
    total_us: AtomicU64,
    /// Number of tasks executed
    count: AtomicU64,
    /// Per-task durations (task_id -> duration_us)
    per_task: RwLock<HashMap<String, u64>>,
}

/// Runtime materialization duration tracking
#[derive(Debug, Default)]
struct RuntimeDurations {
    /// Total materialization time (microseconds)
    total_us: AtomicU64,
    /// Number of runtimes materialized
    count: AtomicU64,
    /// Per-runtime durations (runtime_id -> duration_us)
    per_runtime: RwLock<HashMap<String, u64>>,
}

/// Counters by cache policy
#[derive(Debug, Default)]
struct PolicyCounters {
    normal: AtomicU64,
    readonly: AtomicU64,
    writeonly: AtomicU64,
    disabled: AtomicU64,
}

/// Counters by error type
#[derive(Debug, Default)]
struct ErrorCounters {
    connection: AtomicU64,
    timeout: AtomicU64,
    not_found: AtomicU64,
    digest_mismatch: AtomicU64,
    other: AtomicU64,
}

impl CacheMetrics {
    /// Create new metrics collector
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a cache hit
    pub fn record_hit(&self, policy: CachePolicy, task_id: &str) {
        self.hits.increment(policy);
        tracing::debug!(
            task = %task_id,
            policy = ?policy,
            metric = "cuenv_cache_hit_total",
            "Cache hit recorded"
        );
    }

    /// Record a cache miss
    pub fn record_miss(&self, policy: CachePolicy, task_id: &str) {
        self.misses.increment(policy);
        tracing::debug!(
            task = %task_id,
            policy = ?policy,
            metric = "cuenv_cache_miss_total",
            "Cache miss recorded"
        );
    }

    /// Record a cache restore failure
    pub fn record_restore_failure(&self, error_type: RestoreErrorType, task_id: &str) {
        self.restore_failures.increment(error_type);
        tracing::debug!(
            task = %task_id,
            error_type = ?error_type,
            metric = "cuenv_cache_restore_failure_total",
            "Cache restore failure recorded"
        );
    }

    /// Record bytes downloaded
    pub fn record_download(&self, bytes: u64) {
        self.bytes_downloaded.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Record bytes uploaded
    pub fn record_upload(&self, bytes: u64) {
        self.bytes_uploaded.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Record cache check latency
    pub fn record_check_latency(&self, latency_us: u64) {
        self.check_latency_us.fetch_add(latency_us, Ordering::Relaxed);
        self.check_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Record task execution duration
    pub fn record_task_duration(&self, task_id: &str, duration_ms: u64) {
        let duration_us = duration_ms * 1000;
        self.task_durations.total_us.fetch_add(duration_us, Ordering::Relaxed);
        self.task_durations.count.fetch_add(1, Ordering::Relaxed);

        if let Ok(mut map) = self.task_durations.per_task.write() {
            map.insert(task_id.to_string(), duration_us);
        }

        tracing::debug!(
            task = %task_id,
            duration_ms = duration_ms,
            metric = "cuenv_task_duration_seconds",
            "Task duration recorded"
        );
    }

    /// Record runtime materialization duration
    pub fn record_runtime_materialization(&self, runtime_id: &str, duration_ms: u64) {
        let duration_us = duration_ms * 1000;
        self.runtime_durations.total_us.fetch_add(duration_us, Ordering::Relaxed);
        self.runtime_durations.count.fetch_add(1, Ordering::Relaxed);

        if let Ok(mut map) = self.runtime_durations.per_runtime.write() {
            map.insert(runtime_id.to_string(), duration_us);
        }

        tracing::debug!(
            runtime = %runtime_id,
            duration_ms = duration_ms,
            metric = "cuenv_runtime_materialization_seconds",
            "Runtime materialization recorded"
        );
    }

    /// Get total task execution time in milliseconds
    #[must_use]
    pub fn total_task_time_ms(&self) -> u64 {
        self.task_durations.total_us.load(Ordering::Relaxed) / 1000
    }

    /// Get number of tasks executed
    #[must_use]
    pub fn task_count(&self) -> u64 {
        self.task_durations.count.load(Ordering::Relaxed)
    }

    /// Get average task duration in milliseconds
    #[must_use]
    pub fn avg_task_duration_ms(&self) -> u64 {
        let count = self.task_durations.count.load(Ordering::Relaxed);
        if count == 0 {
            return 0;
        }
        self.task_durations.total_us.load(Ordering::Relaxed) / count / 1000
    }

    /// Get total runtime materialization time in milliseconds
    #[must_use]
    pub fn total_runtime_time_ms(&self) -> u64 {
        self.runtime_durations.total_us.load(Ordering::Relaxed) / 1000
    }

    /// Get number of runtimes materialized
    #[must_use]
    pub fn runtime_count(&self) -> u64 {
        self.runtime_durations.count.load(Ordering::Relaxed)
    }

    /// Get total hits for a policy
    #[must_use]
    pub fn hits(&self, policy: CachePolicy) -> u64 {
        self.hits.get(policy)
    }

    /// Get total misses for a policy
    #[must_use]
    pub fn misses(&self, policy: CachePolicy) -> u64 {
        self.misses.get(policy)
    }

    /// Get total restore failures for an error type
    #[must_use]
    pub fn restore_failures(&self, error_type: RestoreErrorType) -> u64 {
        self.restore_failures.get(error_type)
    }

    /// Get total bytes downloaded
    #[must_use]
    pub fn bytes_downloaded(&self) -> u64 {
        self.bytes_downloaded.load(Ordering::Relaxed)
    }

    /// Get total bytes uploaded
    #[must_use]
    pub fn bytes_uploaded(&self) -> u64 {
        self.bytes_uploaded.load(Ordering::Relaxed)
    }

    /// Get average check latency in microseconds
    #[must_use]
    pub fn avg_check_latency_us(&self) -> u64 {
        let count = self.check_count.load(Ordering::Relaxed);
        if count == 0 {
            return 0;
        }
        self.check_latency_us.load(Ordering::Relaxed) / count
    }

    /// Calculate cache hit rate (0.0 - 1.0)
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let total_hits: u64 = [
            CachePolicy::Normal,
            CachePolicy::Readonly,
            CachePolicy::Writeonly,
            CachePolicy::Disabled,
        ]
        .iter()
        .map(|p| self.hits(*p))
        .sum();

        let total_misses: u64 = [
            CachePolicy::Normal,
            CachePolicy::Readonly,
            CachePolicy::Writeonly,
            CachePolicy::Disabled,
        ]
        .iter()
        .map(|p| self.misses(*p))
        .sum();

        let total = total_hits + total_misses;
        if total == 0 {
            return 0.0;
        }
        total_hits as f64 / total as f64
    }

    /// Export metrics in Prometheus format
    #[must_use]
    pub fn to_prometheus(&self) -> String {
        let mut output = String::new();

        // Cache hits
        output.push_str("# HELP cuenv_cache_hit_total Total number of cache hits\n");
        output.push_str("# TYPE cuenv_cache_hit_total counter\n");
        for policy in &["normal", "readonly", "writeonly", "disabled"] {
            let count = match *policy {
                "normal" => self.hits.normal.load(Ordering::Relaxed),
                "readonly" => self.hits.readonly.load(Ordering::Relaxed),
                "writeonly" => self.hits.writeonly.load(Ordering::Relaxed),
                "disabled" => self.hits.disabled.load(Ordering::Relaxed),
                _ => 0,
            };
            output.push_str(&format!(
                "cuenv_cache_hit_total{{policy=\"{policy}\"}} {count}\n"
            ));
        }

        // Cache misses
        output.push_str("# HELP cuenv_cache_miss_total Total number of cache misses\n");
        output.push_str("# TYPE cuenv_cache_miss_total counter\n");
        for policy in &["normal", "readonly", "writeonly", "disabled"] {
            let count = match *policy {
                "normal" => self.misses.normal.load(Ordering::Relaxed),
                "readonly" => self.misses.readonly.load(Ordering::Relaxed),
                "writeonly" => self.misses.writeonly.load(Ordering::Relaxed),
                "disabled" => self.misses.disabled.load(Ordering::Relaxed),
                _ => 0,
            };
            output.push_str(&format!(
                "cuenv_cache_miss_total{{policy=\"{policy}\"}} {count}\n"
            ));
        }

        // Restore failures
        output.push_str(
            "# HELP cuenv_cache_restore_failure_total Total number of cache restore failures\n",
        );
        output.push_str("# TYPE cuenv_cache_restore_failure_total counter\n");
        for error_type in &["connection", "timeout", "not_found", "digest_mismatch", "other"] {
            let count = match *error_type {
                "connection" => self.restore_failures.connection.load(Ordering::Relaxed),
                "timeout" => self.restore_failures.timeout.load(Ordering::Relaxed),
                "not_found" => self.restore_failures.not_found.load(Ordering::Relaxed),
                "digest_mismatch" => self.restore_failures.digest_mismatch.load(Ordering::Relaxed),
                "other" => self.restore_failures.other.load(Ordering::Relaxed),
                _ => 0,
            };
            output.push_str(&format!(
                "cuenv_cache_restore_failure_total{{error_type=\"{error_type}\"}} {count}\n"
            ));
        }

        // Bytes transferred
        output.push_str("# HELP cuenv_cache_bytes_downloaded_total Total bytes downloaded from cache\n");
        output.push_str("# TYPE cuenv_cache_bytes_downloaded_total counter\n");
        output.push_str(&format!(
            "cuenv_cache_bytes_downloaded_total {}\n",
            self.bytes_downloaded.load(Ordering::Relaxed)
        ));

        output.push_str("# HELP cuenv_cache_bytes_uploaded_total Total bytes uploaded to cache\n");
        output.push_str("# TYPE cuenv_cache_bytes_uploaded_total counter\n");
        output.push_str(&format!(
            "cuenv_cache_bytes_uploaded_total {}\n",
            self.bytes_uploaded.load(Ordering::Relaxed)
        ));

        // Task execution metrics
        output.push_str("# HELP cuenv_task_duration_seconds_total Total task execution time in seconds\n");
        output.push_str("# TYPE cuenv_task_duration_seconds_total counter\n");
        let task_total_secs = self.task_durations.total_us.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        output.push_str(&format!("cuenv_task_duration_seconds_total {task_total_secs:.3}\n"));

        output.push_str("# HELP cuenv_tasks_executed_total Total number of tasks executed\n");
        output.push_str("# TYPE cuenv_tasks_executed_total counter\n");
        output.push_str(&format!(
            "cuenv_tasks_executed_total {}\n",
            self.task_durations.count.load(Ordering::Relaxed)
        ));

        // Runtime materialization metrics
        output.push_str("# HELP cuenv_runtime_materialization_seconds_total Total runtime materialization time in seconds\n");
        output.push_str("# TYPE cuenv_runtime_materialization_seconds_total counter\n");
        let runtime_total_secs = self.runtime_durations.total_us.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        output.push_str(&format!("cuenv_runtime_materialization_seconds_total {runtime_total_secs:.3}\n"));

        output.push_str("# HELP cuenv_runtimes_materialized_total Total number of runtimes materialized\n");
        output.push_str("# TYPE cuenv_runtimes_materialized_total counter\n");
        output.push_str(&format!(
            "cuenv_runtimes_materialized_total {}\n",
            self.runtime_durations.count.load(Ordering::Relaxed)
        ));

        output
    }
}

impl PolicyCounters {
    fn increment(&self, policy: CachePolicy) {
        match policy {
            CachePolicy::Normal => self.normal.fetch_add(1, Ordering::Relaxed),
            CachePolicy::Readonly => self.readonly.fetch_add(1, Ordering::Relaxed),
            CachePolicy::Writeonly => self.writeonly.fetch_add(1, Ordering::Relaxed),
            CachePolicy::Disabled => self.disabled.fetch_add(1, Ordering::Relaxed),
        };
    }

    fn get(&self, policy: CachePolicy) -> u64 {
        match policy {
            CachePolicy::Normal => self.normal.load(Ordering::Relaxed),
            CachePolicy::Readonly => self.readonly.load(Ordering::Relaxed),
            CachePolicy::Writeonly => self.writeonly.load(Ordering::Relaxed),
            CachePolicy::Disabled => self.disabled.load(Ordering::Relaxed),
        }
    }
}

/// Error types for restore failures
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreErrorType {
    /// Connection error
    Connection,
    /// Timeout
    Timeout,
    /// Blob not found
    NotFound,
    /// Digest mismatch
    DigestMismatch,
    /// Other error
    Other,
}

impl ErrorCounters {
    fn increment(&self, error_type: RestoreErrorType) {
        match error_type {
            RestoreErrorType::Connection => self.connection.fetch_add(1, Ordering::Relaxed),
            RestoreErrorType::Timeout => self.timeout.fetch_add(1, Ordering::Relaxed),
            RestoreErrorType::NotFound => self.not_found.fetch_add(1, Ordering::Relaxed),
            RestoreErrorType::DigestMismatch => self.digest_mismatch.fetch_add(1, Ordering::Relaxed),
            RestoreErrorType::Other => self.other.fetch_add(1, Ordering::Relaxed),
        };
    }

    fn get(&self, error_type: RestoreErrorType) -> u64 {
        match error_type {
            RestoreErrorType::Connection => self.connection.load(Ordering::Relaxed),
            RestoreErrorType::Timeout => self.timeout.load(Ordering::Relaxed),
            RestoreErrorType::NotFound => self.not_found.load(Ordering::Relaxed),
            RestoreErrorType::DigestMismatch => self.digest_mismatch.load(Ordering::Relaxed),
            RestoreErrorType::Other => self.other.load(Ordering::Relaxed),
        }
    }
}

/// Global metrics instance
static GLOBAL_METRICS: std::sync::OnceLock<Arc<CacheMetrics>> = std::sync::OnceLock::new();

/// Get or initialize global cache metrics
#[must_use]
pub fn global_metrics() -> Arc<CacheMetrics> {
    GLOBAL_METRICS
        .get_or_init(|| Arc::new(CacheMetrics::new()))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_hit() {
        let metrics = CacheMetrics::new();
        metrics.record_hit(CachePolicy::Normal, "test-task");
        assert_eq!(metrics.hits(CachePolicy::Normal), 1);
        assert_eq!(metrics.hits(CachePolicy::Readonly), 0);
    }

    #[test]
    fn test_record_miss() {
        let metrics = CacheMetrics::new();
        metrics.record_miss(CachePolicy::Readonly, "test-task");
        assert_eq!(metrics.misses(CachePolicy::Readonly), 1);
        assert_eq!(metrics.misses(CachePolicy::Normal), 0);
    }

    #[test]
    fn test_record_restore_failure() {
        let metrics = CacheMetrics::new();
        metrics.record_restore_failure(RestoreErrorType::Connection, "test-task");
        assert_eq!(metrics.restore_failures(RestoreErrorType::Connection), 1);
        assert_eq!(metrics.restore_failures(RestoreErrorType::Timeout), 0);
    }

    #[test]
    fn test_hit_rate() {
        let metrics = CacheMetrics::new();
        metrics.record_hit(CachePolicy::Normal, "t1");
        metrics.record_hit(CachePolicy::Normal, "t2");
        metrics.record_hit(CachePolicy::Normal, "t3");
        metrics.record_miss(CachePolicy::Normal, "t4");

        let rate = metrics.hit_rate();
        assert!((rate - 0.75).abs() < 0.001);
    }

    #[test]
    fn test_hit_rate_zero() {
        let metrics = CacheMetrics::new();
        assert_eq!(metrics.hit_rate(), 0.0);
    }

    #[test]
    fn test_bytes_tracking() {
        let metrics = CacheMetrics::new();
        metrics.record_download(1000);
        metrics.record_upload(500);
        assert_eq!(metrics.bytes_downloaded(), 1000);
        assert_eq!(metrics.bytes_uploaded(), 500);
    }

    #[test]
    fn test_prometheus_format() {
        let metrics = CacheMetrics::new();
        metrics.record_hit(CachePolicy::Normal, "t1");
        metrics.record_miss(CachePolicy::Normal, "t2");

        let output = metrics.to_prometheus();
        assert!(output.contains("cuenv_cache_hit_total"));
        assert!(output.contains("cuenv_cache_miss_total"));
        assert!(output.contains("policy=\"normal\""));
    }

    #[test]
    fn test_avg_latency() {
        let metrics = CacheMetrics::new();
        metrics.record_check_latency(100);
        metrics.record_check_latency(200);
        metrics.record_check_latency(300);
        assert_eq!(metrics.avg_check_latency_us(), 200);
    }
}
