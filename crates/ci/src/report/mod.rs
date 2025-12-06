use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineReport {
    pub version: String,
    pub project: String,
    pub pipeline: String,
    pub context: ContextReport,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    pub status: PipelineStatus,
    pub tasks: Vec<TaskReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextReport {
    pub provider: String,
    pub event: String,
    pub ref_name: String,
    pub base_ref: Option<String>,
    pub sha: String,
    pub changed_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskReport {
    pub name: String,
    pub status: TaskStatus,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub inputs_matched: Vec<String>,
    pub cache_key: Option<String>,
    pub outputs: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Success,
    Failed,
    Cached,
    Skipped,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PipelineStatus {
    Pending,
    Success,
    Failed,
    Partial,
}

/// Opaque handle for a check run (provider specific)
#[derive(Debug, Clone)]
pub struct CheckHandle {
    pub id: String,
}

pub mod json;
pub mod markdown;
