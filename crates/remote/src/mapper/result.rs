//! Mapper from REAPI ActionResult to cuenv TaskResult

use crate::client::CasClient;
use crate::error::Result;
use crate::merkle::Digest;
use crate::reapi::ActionResult;
use cuenv_core::tasks::TaskResult;

/// Mapper for converting REAPI ActionResult to cuenv TaskResult
pub struct ResultMapper;

impl ResultMapper {
    /// Map an REAPI ActionResult to a cuenv TaskResult
    ///
    /// Extracts stdout/stderr from the ActionResult, preferring inline data
    /// and falling back to CAS download if needed.
    ///
    /// # Arguments
    /// * `task_name` - Name of the task for the result
    /// * `action_result` - The REAPI ActionResult from execution
    /// * `cas_client` - Optional CAS client for downloading large outputs
    pub async fn map_result(
        task_name: &str,
        action_result: ActionResult,
        cas_client: Option<&CasClient>,
    ) -> Result<TaskResult> {
        // Extract stdout - prefer inline data, fall back to CAS download
        let stdout = Self::extract_output(
            &action_result.stdout_raw,
            action_result.stdout_digest.as_ref(),
            cas_client,
        )
        .await?;

        // Extract stderr - prefer inline data, fall back to CAS download
        let stderr = Self::extract_output(
            &action_result.stderr_raw,
            action_result.stderr_digest.as_ref(),
            cas_client,
        )
        .await?;

        let success = action_result.exit_code == 0;

        Ok(TaskResult {
            name: task_name.to_string(),
            exit_code: Some(action_result.exit_code),
            stdout,
            stderr,
            success,
        })
    }

    /// Extract output from inline data or CAS
    async fn extract_output(
        inline_data: &[u8],
        digest: Option<&crate::reapi::Digest>,
        cas_client: Option<&CasClient>,
    ) -> Result<String> {
        // Prefer inline data if available
        if !inline_data.is_empty() {
            return Ok(String::from_utf8_lossy(inline_data).to_string());
        }

        // Try to download from CAS if we have a digest and client
        if let (Some(proto_digest), Some(client)) = (digest, cas_client) {
            let digest = Digest::new(&proto_digest.hash, proto_digest.size_bytes)?;
            let data = client.read_blob(&digest).await?;
            return Ok(String::from_utf8_lossy(&data).to_string());
        }

        // No data available
        Ok(String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_map_result_inline_output() {
        let action_result = ActionResult {
            exit_code: 0,
            stdout_raw: b"hello stdout".to_vec(),
            stderr_raw: b"hello stderr".to_vec(),
            stdout_digest: None,
            stderr_digest: None,
            output_files: vec![],
            output_file_symlinks: vec![],
            output_symlinks: vec![],
            output_directories: vec![],
            output_directory_symlinks: vec![],
            execution_metadata: None,
        };

        let result = ResultMapper::map_result("test_task", action_result, None)
            .await
            .unwrap();

        assert_eq!(result.name, "test_task");
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.stdout, "hello stdout");
        assert_eq!(result.stderr, "hello stderr");
        assert!(result.success);
    }

    #[tokio::test]
    async fn test_map_result_failure() {
        let action_result = ActionResult {
            exit_code: 1,
            stdout_raw: vec![],
            stderr_raw: b"error message".to_vec(),
            stdout_digest: None,
            stderr_digest: None,
            output_files: vec![],
            output_file_symlinks: vec![],
            output_symlinks: vec![],
            output_directories: vec![],
            output_directory_symlinks: vec![],
            execution_metadata: None,
        };

        let result = ResultMapper::map_result("failing_task", action_result, None)
            .await
            .unwrap();

        assert_eq!(result.name, "failing_task");
        assert_eq!(result.exit_code, Some(1));
        assert_eq!(result.stderr, "error message");
        assert!(!result.success);
    }
}
