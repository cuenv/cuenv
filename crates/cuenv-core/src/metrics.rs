//! Production metrics and observability
//!
//! This module provides a functional approach to metrics collection and export
//! for production monitoring and observability.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Metrics collector for production observability
#[derive(Debug, Clone)]
pub struct MetricsCollector {
    state: Arc<MetricsState>,
}

/// Internal metrics state - immutable after construction
#[derive(Debug)]
struct MetricsState {
    // Task execution metrics
    tasks_total: AtomicU64,
    tasks_succeeded: AtomicU64,
    tasks_failed: AtomicU64,
    tasks_cache_hits: AtomicU64,
    tasks_cache_misses: AtomicU64,
    task_execution_time_ms: AtomicU64,

    // Hook execution metrics
    hooks_total: AtomicU64,
    hooks_succeeded: AtomicU64,
    hooks_failed: AtomicU64,
    hook_execution_time_ms: AtomicU64,

    // Resource metrics
    concurrent_tasks: AtomicU64,
    concurrent_hooks: AtomicU64,
    peak_concurrent_tasks: AtomicU64,
    peak_concurrent_hooks: AtomicU64,

    // Startup time
    start_time: Instant,
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsCollector {
    /// Create a new metrics collector
    pub fn new() -> Self {
        Self {
            state: Arc::new(MetricsState {
                tasks_total: AtomicU64::new(0),
                tasks_succeeded: AtomicU64::new(0),
                tasks_failed: AtomicU64::new(0),
                tasks_cache_hits: AtomicU64::new(0),
                tasks_cache_misses: AtomicU64::new(0),
                task_execution_time_ms: AtomicU64::new(0),
                hooks_total: AtomicU64::new(0),
                hooks_succeeded: AtomicU64::new(0),
                hooks_failed: AtomicU64::new(0),
                hook_execution_time_ms: AtomicU64::new(0),
                concurrent_tasks: AtomicU64::new(0),
                concurrent_hooks: AtomicU64::new(0),
                peak_concurrent_tasks: AtomicU64::new(0),
                peak_concurrent_hooks: AtomicU64::new(0),
                start_time: Instant::now(),
            }),
        }
    }

    /// Record a task execution - functional approach with RAII guard
    pub fn record_task_execution(&self, cache_hit: bool) -> TaskExecutionGuard {
        self.state.tasks_total.fetch_add(1, Ordering::Relaxed);
        if cache_hit {
            self.state.tasks_cache_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.state
                .tasks_cache_misses
                .fetch_add(1, Ordering::Relaxed);
        }

        // Track concurrent tasks
        let current = self.state.concurrent_tasks.fetch_add(1, Ordering::Relaxed) + 1;
        self.state
            .peak_concurrent_tasks
            .fetch_max(current, Ordering::Relaxed);

        TaskExecutionGuard {
            state: Arc::clone(&self.state),
            start: Instant::now(),
            completed: false,
        }
    }

    /// Record a hook execution - functional approach with RAII guard
    pub fn record_hook_execution(&self) -> HookExecutionGuard {
        self.state.hooks_total.fetch_add(1, Ordering::Relaxed);

        // Track concurrent hooks
        let current = self.state.concurrent_hooks.fetch_add(1, Ordering::Relaxed) + 1;
        self.state
            .peak_concurrent_hooks
            .fetch_max(current, Ordering::Relaxed);

        HookExecutionGuard {
            state: Arc::clone(&self.state),
            start: Instant::now(),
            completed: false,
        }
    }

    /// Export metrics in Prometheus text format
    pub fn export_prometheus(&self) -> String {
        let uptime = self.state.start_time.elapsed().as_secs();

        format!(
            r#"# HELP cuenv_tasks_total Total number of tasks executed
# TYPE cuenv_tasks_total counter
cuenv_tasks_total {{}} {}

# HELP cuenv_tasks_succeeded Number of tasks that succeeded
# TYPE cuenv_tasks_succeeded counter
cuenv_tasks_succeeded {{}} {}

# HELP cuenv_tasks_failed Number of tasks that failed
# TYPE cuenv_tasks_failed counter
cuenv_tasks_failed {{}} {}

# HELP cuenv_tasks_cache_hits Number of task cache hits
# TYPE cuenv_tasks_cache_hits counter
cuenv_tasks_cache_hits {{}} {}

# HELP cuenv_tasks_cache_misses Number of task cache misses
# TYPE cuenv_tasks_cache_misses counter
cuenv_tasks_cache_misses {{}} {}

# HELP cuenv_task_execution_time_ms_total Total task execution time in milliseconds
# TYPE cuenv_task_execution_time_ms_total counter
cuenv_task_execution_time_ms_total {{}} {}

# HELP cuenv_hooks_total Total number of hooks executed
# TYPE cuenv_hooks_total counter
cuenv_hooks_total {{}} {}

# HELP cuenv_hooks_succeeded Number of hooks that succeeded
# TYPE cuenv_hooks_succeeded counter
cuenv_hooks_succeeded {{}} {}

# HELP cuenv_hooks_failed Number of hooks that failed
# TYPE cuenv_hooks_failed counter
cuenv_hooks_failed {{}} {}

# HELP cuenv_hook_execution_time_ms_total Total hook execution time in milliseconds
# TYPE cuenv_hook_execution_time_ms_total counter
cuenv_hook_execution_time_ms_total {{}} {}

# HELP cuenv_concurrent_tasks Current number of concurrent tasks
# TYPE cuenv_concurrent_tasks gauge
cuenv_concurrent_tasks {{}} {}

# HELP cuenv_concurrent_hooks Current number of concurrent hooks
# TYPE cuenv_concurrent_hooks gauge
cuenv_concurrent_hooks {{}} {}

# HELP cuenv_peak_concurrent_tasks Peak number of concurrent tasks
# TYPE cuenv_peak_concurrent_tasks gauge
cuenv_peak_concurrent_tasks {{}} {}

# HELP cuenv_peak_concurrent_hooks Peak number of concurrent hooks
# TYPE cuenv_peak_concurrent_hooks gauge
cuenv_peak_concurrent_hooks {{}} {}

# HELP cuenv_uptime_seconds Uptime in seconds
# TYPE cuenv_uptime_seconds counter
cuenv_uptime_seconds {{}} {}
"#,
            self.state.tasks_total.load(Ordering::Relaxed),
            self.state.tasks_succeeded.load(Ordering::Relaxed),
            self.state.tasks_failed.load(Ordering::Relaxed),
            self.state.tasks_cache_hits.load(Ordering::Relaxed),
            self.state.tasks_cache_misses.load(Ordering::Relaxed),
            self.state.task_execution_time_ms.load(Ordering::Relaxed),
            self.state.hooks_total.load(Ordering::Relaxed),
            self.state.hooks_succeeded.load(Ordering::Relaxed),
            self.state.hooks_failed.load(Ordering::Relaxed),
            self.state.hook_execution_time_ms.load(Ordering::Relaxed),
            self.state.concurrent_tasks.load(Ordering::Relaxed),
            self.state.concurrent_hooks.load(Ordering::Relaxed),
            self.state.peak_concurrent_tasks.load(Ordering::Relaxed),
            self.state.peak_concurrent_hooks.load(Ordering::Relaxed),
            uptime,
        )
    }

    /// Export metrics as JSON for structured logging
    pub fn export_json(&self) -> String {
        let uptime = self.state.start_time.elapsed().as_secs();

        serde_json::json!({
            "tasks": {
                "total": self.state.tasks_total.load(Ordering::Relaxed),
                "succeeded": self.state.tasks_succeeded.load(Ordering::Relaxed),
                "failed": self.state.tasks_failed.load(Ordering::Relaxed),
                "cache_hits": self.state.tasks_cache_hits.load(Ordering::Relaxed),
                "cache_misses": self.state.tasks_cache_misses.load(Ordering::Relaxed),
                "execution_time_ms": self.state.task_execution_time_ms.load(Ordering::Relaxed),
            },
            "hooks": {
                "total": self.state.hooks_total.load(Ordering::Relaxed),
                "succeeded": self.state.hooks_succeeded.load(Ordering::Relaxed),
                "failed": self.state.hooks_failed.load(Ordering::Relaxed),
                "execution_time_ms": self.state.hook_execution_time_ms.load(Ordering::Relaxed),
            },
            "concurrency": {
                "current_tasks": self.state.concurrent_tasks.load(Ordering::Relaxed),
                "current_hooks": self.state.concurrent_hooks.load(Ordering::Relaxed),
                "peak_tasks": self.state.peak_concurrent_tasks.load(Ordering::Relaxed),
                "peak_hooks": self.state.peak_concurrent_hooks.load(Ordering::Relaxed),
            },
            "uptime_seconds": uptime,
        })
        .to_string()
    }

    /// Get a summary snapshot for display
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            tasks_total: self.state.tasks_total.load(Ordering::Relaxed),
            tasks_succeeded: self.state.tasks_succeeded.load(Ordering::Relaxed),
            tasks_failed: self.state.tasks_failed.load(Ordering::Relaxed),
            tasks_cache_hit_rate: {
                let hits = self.state.tasks_cache_hits.load(Ordering::Relaxed);
                let total = hits + self.state.tasks_cache_misses.load(Ordering::Relaxed);
                if total > 0 {
                    (hits as f64 / total as f64) * 100.0
                } else {
                    0.0
                }
            },
            hooks_total: self.state.hooks_total.load(Ordering::Relaxed),
            hooks_succeeded: self.state.hooks_succeeded.load(Ordering::Relaxed),
            hooks_failed: self.state.hooks_failed.load(Ordering::Relaxed),
            uptime: self.state.start_time.elapsed(),
        }
    }
}

/// RAII guard for task execution - automatically records success/failure on drop
pub struct TaskExecutionGuard {
    state: Arc<MetricsState>,
    start: Instant,
    completed: bool,
}

impl TaskExecutionGuard {
    /// Mark the task as successful
    pub fn success(mut self) {
        self.state.tasks_succeeded.fetch_add(1, Ordering::Relaxed);
        let duration_ms = self.start.elapsed().as_millis() as u64;
        self.state
            .task_execution_time_ms
            .fetch_add(duration_ms, Ordering::Relaxed);
        self.state.concurrent_tasks.fetch_sub(1, Ordering::Relaxed);
        self.completed = true;
    }

    /// Mark the task as failed
    pub fn failure(mut self) {
        self.state.tasks_failed.fetch_add(1, Ordering::Relaxed);
        let duration_ms = self.start.elapsed().as_millis() as u64;
        self.state
            .task_execution_time_ms
            .fetch_add(duration_ms, Ordering::Relaxed);
        self.state.concurrent_tasks.fetch_sub(1, Ordering::Relaxed);
        self.completed = true;
    }
}

impl Drop for TaskExecutionGuard {
    fn drop(&mut self) {
        // If not explicitly marked as success or failure, count as failure
        if !self.completed {
            self.state.tasks_failed.fetch_add(1, Ordering::Relaxed);
            let duration_ms = self.start.elapsed().as_millis() as u64;
            self.state
                .task_execution_time_ms
                .fetch_add(duration_ms, Ordering::Relaxed);
            self.state.concurrent_tasks.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// RAII guard for hook execution - automatically records success/failure on drop
pub struct HookExecutionGuard {
    state: Arc<MetricsState>,
    start: Instant,
    completed: bool,
}

impl HookExecutionGuard {
    /// Mark the hook as successful
    pub fn success(mut self) {
        self.state.hooks_succeeded.fetch_add(1, Ordering::Relaxed);
        let duration_ms = self.start.elapsed().as_millis() as u64;
        self.state
            .hook_execution_time_ms
            .fetch_add(duration_ms, Ordering::Relaxed);
        self.state.concurrent_hooks.fetch_sub(1, Ordering::Relaxed);
        self.completed = true;
    }

    /// Mark the hook as failed
    pub fn failure(mut self) {
        self.state.hooks_failed.fetch_add(1, Ordering::Relaxed);
        let duration_ms = self.start.elapsed().as_millis() as u64;
        self.state
            .hook_execution_time_ms
            .fetch_add(duration_ms, Ordering::Relaxed);
        self.state.concurrent_hooks.fetch_sub(1, Ordering::Relaxed);
        self.completed = true;
    }
}

impl Drop for HookExecutionGuard {
    fn drop(&mut self) {
        // If not explicitly marked as success or failure, count as failure
        if !self.completed {
            self.state.hooks_failed.fetch_add(1, Ordering::Relaxed);
            let duration_ms = self.start.elapsed().as_millis() as u64;
            self.state
                .hook_execution_time_ms
                .fetch_add(duration_ms, Ordering::Relaxed);
            self.state.concurrent_hooks.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// Immutable snapshot of metrics at a point in time
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub tasks_total: u64,
    pub tasks_succeeded: u64,
    pub tasks_failed: u64,
    pub tasks_cache_hit_rate: f64,
    pub hooks_total: u64,
    pub hooks_succeeded: u64,
    pub hooks_failed: u64,
    pub uptime: Duration,
}

impl std::fmt::Display for MetricsSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Tasks: {}/{} succeeded ({:.1}% cache hit rate) | Hooks: {}/{} succeeded | Uptime: {}s",
            self.tasks_succeeded,
            self.tasks_total,
            self.tasks_cache_hit_rate,
            self.hooks_succeeded,
            self.hooks_total,
            self.uptime.as_secs(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_execution_guard_success() {
        let metrics = MetricsCollector::new();
        let guard = metrics.record_task_execution(false);
        guard.success();

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.tasks_total, 1);
        assert_eq!(snapshot.tasks_succeeded, 1);
        assert_eq!(snapshot.tasks_failed, 0);
    }

    #[test]
    fn test_task_execution_guard_failure() {
        let metrics = MetricsCollector::new();
        let guard = metrics.record_task_execution(false);
        guard.failure();

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.tasks_total, 1);
        assert_eq!(snapshot.tasks_succeeded, 0);
        assert_eq!(snapshot.tasks_failed, 1);
    }

    #[test]
    fn test_cache_hit_rate() {
        let metrics = MetricsCollector::new();

        // 3 cache hits
        for _ in 0..3 {
            let guard = metrics.record_task_execution(true);
            guard.success();
        }

        // 1 cache miss
        let guard = metrics.record_task_execution(false);
        guard.success();

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.tasks_cache_hit_rate, 75.0);
    }

    #[test]
    fn test_prometheus_export() {
        let metrics = MetricsCollector::new();
        let guard = metrics.record_task_execution(false);
        guard.success();

        let prometheus_text = metrics.export_prometheus();
        assert!(prometheus_text.contains("cuenv_tasks_total"));
        assert!(prometheus_text.contains("cuenv_tasks_succeeded"));
    }

    #[test]
    fn test_json_export() {
        let metrics = MetricsCollector::new();
        let guard = metrics.record_task_execution(false);
        guard.success();

        let json_text = metrics.export_json();
        let parsed: serde_json::Value = serde_json::from_str(&json_text).unwrap();
        assert_eq!(parsed["tasks"]["total"], 1);
        assert_eq!(parsed["tasks"]["succeeded"], 1);
    }
}
