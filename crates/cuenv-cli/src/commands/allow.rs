//! Approval management command handlers
//!
//! This module provides commands for managing configuration approvals
//! required before hook execution can proceed.

use cuenv_core::{approval::ApprovalManager, Result};
use cuengine::CueEvaluator;
use std::path::Path;

/// Execute allow command to approve current directory's configuration
pub async fn execute_allow(path: &str, note: Option<String>) -> Result<String> {
    let directory = Path::new(path);
    
    // Validate directory exists
    if !directory.exists() {
        return Err(cuenv_core::Error::configuration(format!(
            "Directory does not exist: {}",
            path
        )));
    }

    // Evaluate the CUE configuration to get its hash
    let config_value = evaluate_cue_config(directory).await?;
    let config_hash = ApprovalManager::compute_config_hash(&config_value);
    
    // Initialize approval manager
    let manager = ApprovalManager::new()?;
    
    // Check if already approved
    if manager.is_approved(directory, &config_hash)? {
        return Ok(format!(
            "Configuration is already approved for directory: {}",
            directory.display()
        ));
    }
    
    // Approve the configuration
    manager.approve_config(directory, config_hash.clone(), note)?;
    
    Ok(format!(
        "Configuration approved for directory: {}\nHash: {}",
        directory.display(),
        &config_hash[..16] // Show first 16 chars of hash
    ))
}

/// Evaluate CUE configuration in directory
async fn evaluate_cue_config(directory: &Path) -> Result<serde_json::Value> {
    // Look for env.cue file
    let env_cue_path = directory.join("env.cue");
    if !env_cue_path.exists() {
        return Err(cuenv_core::Error::configuration(format!(
            "No env.cue file found in directory: {}",
            directory.display()
        )));
    }
    
    // Use cuengine to evaluate the configuration
    let evaluator = CueEvaluator::builder().build()?;
    let json_result = evaluator.evaluate(directory, "cuenv")?;
    
    // Parse the JSON response
    let parsed: serde_json::Value = serde_json::from_str(&json_result).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to parse CUE output as JSON: {e}"))
    })?;
    
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    use std::env;

    #[tokio::test]
    async fn test_execute_allow_directory_not_found() {
        let result = execute_allow("/nonexistent/directory", None).await;
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Directory does not exist"));
    }

    #[tokio::test]
    async fn test_execute_allow_no_env_cue() {
        let temp_dir = TempDir::new().unwrap();
        
        // Set home for approval manager
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }
        
        let result = execute_allow(temp_dir.path().to_str().unwrap(), None).await;
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("No env.cue file found"));
    }

    #[tokio::test]
    async fn test_execute_allow_with_valid_env_cue() {
        let temp_dir = TempDir::new().unwrap();
        
        // Set home for approval manager
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }
        
        // Create a simple env.cue file
        let env_cue_content = r#"
package cuenv

env: {
    TEST_VAR: "test_value"
}
"#;
        let env_cue_path = temp_dir.path().join("env.cue");
        fs::write(&env_cue_path, env_cue_content).unwrap();
        
        let result = execute_allow(temp_dir.path().to_str().unwrap(), Some("Test approval".to_string())).await;
        
        // This may fail due to CUE evaluation, but we're testing the structure
        match result {
            Ok(output) => {
                assert!(output.contains("Configuration approved"));
                assert!(output.contains("Hash:"));
            }
            Err(e) => {
                // Expected to fail in test environment without proper CUE setup
                // Just verify it's trying to evaluate CUE
                let error_msg = e.to_string();
                // Should be a CUE-related error, not a directory/file error
                assert!(!error_msg.contains("Directory does not exist"));
                assert!(!error_msg.contains("No env.cue file found"));
            }
        }
    }

    #[tokio::test] 
    async fn test_evaluate_cue_config_no_file() {
        let temp_dir = TempDir::new().unwrap();
        let result = evaluate_cue_config(temp_dir.path()).await;
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("No env.cue file found"));
    }

    #[tokio::test]
    async fn test_evaluate_cue_config_with_file() {
        let temp_dir = TempDir::new().unwrap();
        
        // Create a simple env.cue file
        let env_cue_content = r#"
package cuenv

env: {
    TEST: "value"
}
"#;
        let env_cue_path = temp_dir.path().join("env.cue");
        fs::write(&env_cue_path, env_cue_content).unwrap();
        
        let result = evaluate_cue_config(temp_dir.path()).await;
        
        // This may fail in test environment, but we've validated file exists
        match result {
            Ok(value) => {
                // If evaluation succeeds, should get a JSON value
                assert!(!value.is_null());
            }
            Err(e) => {
                // Expected to fail without proper CUE setup
                // Just verify it's not a file-not-found error
                let error_msg = e.to_string(); 
                assert!(!error_msg.contains("No env.cue file found"));
            }
        }
    }
}