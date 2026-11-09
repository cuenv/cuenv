//! The non-payload portion of hook state stored on disk.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::HookExecutionState;
use crate::ExecutionStatus;

/// Hook progress metadata; hook arguments, output and environment stay in memory.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PersistedHookState {
    pub(super) format_version: u8,
    pub(super) instance_hash: String,
    pub(super) session_id: Uuid,
    pub(super) run_id: Uuid,
    pub(super) directory_path: PathBuf,
    pub(super) config_hash: String,
    pub(super) status: ExecutionStatus,
    pub(super) total_hooks: usize,
    pub(super) completed_hooks: usize,
    pub(super) current_hook_index: Option<usize>,
    pub(super) started_at: DateTime<Utc>,
    pub(super) finished_at: Option<DateTime<Utc>>,
    pub(super) current_hook_started_at: Option<DateTime<Utc>>,
    pub(super) completed_display_until: Option<DateTime<Utc>>,
}

impl From<&HookExecutionState> for PersistedHookState {
    fn from(state: &HookExecutionState) -> Self {
        Self {
            format_version: 2,
            instance_hash: state.instance_hash.clone(),
            session_id: state.session_id,
            run_id: state.run_id,
            directory_path: state.directory_path.clone(),
            config_hash: state.config_hash.clone(),
            status: state.status.clone(),
            total_hooks: state.total_hooks,
            completed_hooks: state.completed_hooks,
            current_hook_index: state.current_hook_index,
            started_at: state.started_at,
            finished_at: state.finished_at,
            current_hook_started_at: state.current_hook_started_at,
            completed_display_until: state.completed_display_until,
        }
    }
}

impl From<PersistedHookState> for HookExecutionState {
    fn from(state: PersistedHookState) -> Self {
        let error_message = match &state.status {
            ExecutionStatus::Failed => Some("Hook execution failed".to_string()),
            ExecutionStatus::Cancelled => Some("Hook execution was cancelled".to_string()),
            _ => None,
        };
        Self {
            instance_hash: state.instance_hash,
            session_id: state.session_id,
            run_id: state.run_id,
            directory_path: state.directory_path,
            config_hash: state.config_hash,
            status: state.status,
            total_hooks: state.total_hooks,
            completed_hooks: state.completed_hooks,
            current_hook_index: state.current_hook_index,
            hooks: Vec::new(),
            hook_results: HashMap::new(),
            started_at: state.started_at,
            finished_at: state.finished_at,
            current_hook_started_at: state.current_hook_started_at,
            completed_display_until: state.completed_display_until,
            error_message,
            environment_vars: HashMap::new(),
        }
    }
}
