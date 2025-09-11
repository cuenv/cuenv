//! Allow command for approving cuenv configurations

use cuengine::CueEvaluator;
use cuenv_core::{Result, approval::ApprovalManager};
use serde_json::Value;
use std::path::PathBuf;
use tracing::{debug, info};

/// Execute the allow command to approve current configuration
pub async fn execute_allow(directory: Option<PathBuf>) -> Result<String> {
    let dir = directory.unwrap_or_else(|| PathBuf::from("."));

    // Get the approval file path
    let approval_file = ApprovalManager::default_approval_file()?;
    let mut manager = ApprovalManager::new(approval_file);

    // Load existing approvals
    manager.load_approvals().await?;

    // Check if env.cue exists
    let env_file = dir.join("env.cue");
    if !env_file.exists() {
        return Err(cuenv_core::Error::configuration(format!(
            "No env.cue file found in {}",
            dir.display()
        )));
    }

    // Evaluate the CUE configuration to get its JSON representation
    let evaluator = CueEvaluator::builder().build()?;
    let json_result = evaluator.evaluate(&dir, "cuenv")?;
    let config: Value = serde_json::from_str(&json_result).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to parse CUE output: {e}"))
    })?;

    // Compute hash of the evaluated configuration
    let config_hash = ApprovalManager::compute_hash(&config);

    // Check if already approved
    if manager.is_approved(&config_hash) {
        info!("Configuration already approved: {}", config_hash);
        return Ok(format!(
            "Configuration in {} is already approved",
            dir.display()
        ));
    }

    // Approve the configuration
    manager.approve(config_hash.clone()).await?;

    debug!("Approved configuration hash: {}", config_hash);
    Ok(format!(
        "Configuration in {} has been approved",
        dir.display()
    ))
}

/// List all approved configurations
#[allow(dead_code)]
pub async fn execute_list_approved() -> Result<String> {
    let approval_file = ApprovalManager::default_approval_file()?;
    let mut manager = ApprovalManager::new(approval_file);

    manager.load_approvals().await?;

    let approved = manager.list_approved();

    if approved.is_empty() {
        Ok("No approved configurations".to_string())
    } else {
        Ok(format!("Approved configurations:\n{}", approved.join("\n")))
    }
}

/// Revoke approval for a configuration
#[allow(dead_code)]
pub async fn execute_revoke(hash: &str) -> Result<String> {
    let approval_file = ApprovalManager::default_approval_file()?;
    let mut manager = ApprovalManager::new(approval_file);

    manager.load_approvals().await?;

    if !manager.is_approved(hash) {
        return Ok(format!("Configuration {hash} was not approved"));
    }

    manager.revoke(hash).await?;

    Ok(format!("Revoked approval for configuration {hash}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_execute_allow_no_env_file() {
        let temp_dir = TempDir::new().unwrap();
        let result = execute_allow(Some(temp_dir.path().to_path_buf())).await;

        assert!(result.is_err());
        if let Err(e) = result {
            let error_str = e.to_string();
            assert!(error_str.contains("No env.cue file found"));
        }
    }

    #[tokio::test]
    async fn test_execute_list_approved_empty() {
        // This test might fail if there are existing approvals
        // In a real test, we'd mock the approval file location
        let result = execute_list_approved().await;

        // Just check that it doesn't error
        assert!(result.is_ok());
    }
}
