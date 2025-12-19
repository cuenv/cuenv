use crate::reapi::build::bazel::remote::execution::v2 as reapi;
use cuenv_core::tasks::TaskResult;

pub struct ResultMapper;

impl ResultMapper {
    pub fn map_result(name: &str, action_result: reapi::ActionResult) -> TaskResult {
        TaskResult {
            name: name.to_string(),
            exit_code: Some(action_result.exit_code),
            stdout: String::from_utf8_lossy(&action_result.stdout_raw).to_string(),
            stderr: String::from_utf8_lossy(&action_result.stderr_raw).to_string(),
            success: action_result.exit_code == 0,
        }
    }
}