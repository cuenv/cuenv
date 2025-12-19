//! Mapper from REAPI ActionResult to cuenv TaskResult

use crate::client::action_cache::ActionResult;
use crate::error::Result;
use cuenv_core::tasks::TaskResult;

/// Mapper for converting REAPI ActionResult to cuenv TaskResult
pub struct ResultMapper;

impl ResultMapper {
    /// Map an REAPI ActionResult to a cuenv TaskResult
    ///
    /// Phase 3: Implement full mapping:
    /// 1. Download stdout/stderr blobs from CAS if they exist
    /// 2. Extract exit code
    /// 3. Build TaskResult
    pub async fn map_result(
        _task_name: &str,
        action_result: ActionResult,
        _download_outputs: bool,
    ) -> Result<TaskResult> {
        // TODO: In Phase 3, download stdout/stderr from CAS

        Ok(TaskResult {
            name: _task_name.to_string(),
            exit_code: Some(action_result.exit_code),
            stdout: String::new(), // TODO: download from CAS
            stderr: String::new(), // TODO: download from CAS
            success: action_result.exit_code == 0,
        })
    }
}
