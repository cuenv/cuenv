//! Approval system for cuenv configurations
//!
//! This module provides hash-based approval for configurations to ensure
//! that only explicitly approved configurations are executed.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::PathBuf;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

/// Manages configuration approvals
pub struct ApprovalManager {
    /// Path to the approval file
    approval_file: PathBuf,
    /// In-memory cache of approved hashes
    approved_hashes: HashSet<String>,
}

impl ApprovalManager {
    /// Create a new approval manager
    pub fn new(approval_file: PathBuf) -> Self {
        Self {
            approval_file,
            approved_hashes: HashSet::new(),
        }
    }

    /// Get the default approval file path (~/.cuenv/approved.json)
    pub fn default_approval_file() -> Result<PathBuf> {
        let home = dirs::home_dir()
            .ok_or_else(|| Error::configuration("Could not determine home directory"))?;
        Ok(home.join(".cuenv").join("approved.json"))
    }

    /// Load approved hashes from disk
    pub async fn load_approvals(&mut self) -> Result<()> {
        if !self.approval_file.exists() {
            debug!(
                "No approval file found at: {}",
                self.approval_file.display()
            );
            // Create parent directory if it doesn't exist
            if let Some(parent) = self.approval_file.parent() {
                fs::create_dir_all(parent).await.map_err(|e| Error::Io {
                    source: e,
                    path: Some(parent.to_path_buf().into_boxed_path()),
                    operation: "create approval directory".to_string(),
                })?;
            }
            return Ok(());
        }

        debug!("Loading approvals from: {}", self.approval_file.display());

        let contents = fs::read_to_string(&self.approval_file)
            .await
            .map_err(|e| Error::Io {
                source: e,
                path: Some(self.approval_file.clone().into_boxed_path()),
                operation: "read approval file".to_string(),
            })?;

        let approvals: ApprovalData = serde_json::from_str(&contents)
            .map_err(|e| Error::configuration(format!("Failed to parse approval file: {}", e)))?;

        self.approved_hashes = approvals.hashes.into_iter().collect();
        info!(
            "Loaded {} approved configurations",
            self.approved_hashes.len()
        );

        Ok(())
    }

    /// Save approved hashes to disk
    pub async fn save_approvals(&self) -> Result<()> {
        debug!("Saving approvals to: {}", self.approval_file.display());

        let approvals = ApprovalData {
            version: 1,
            hashes: self.approved_hashes.iter().cloned().collect(),
            metadata: ApprovalMetadata {
                last_updated: chrono::Utc::now(),
            },
        };

        let json = serde_json::to_string_pretty(&approvals)
            .map_err(|e| Error::configuration(format!("Failed to serialize approvals: {}", e)))?;

        // Ensure parent directory exists
        if let Some(parent) = self.approval_file.parent() {
            fs::create_dir_all(parent).await.map_err(|e| Error::Io {
                source: e,
                path: Some(parent.to_path_buf().into_boxed_path()),
                operation: "create approval directory".to_string(),
            })?;
        }

        let mut file = fs::File::create(&self.approval_file)
            .await
            .map_err(|e| Error::Io {
                source: e,
                path: Some(self.approval_file.clone().into_boxed_path()),
                operation: "create approval file".to_string(),
            })?;

        file.write_all(json.as_bytes())
            .await
            .map_err(|e| Error::Io {
                source: e,
                path: Some(self.approval_file.clone().into_boxed_path()),
                operation: "write approval file".to_string(),
            })?;

        info!(
            "Saved {} approved configurations",
            self.approved_hashes.len()
        );
        Ok(())
    }

    /// Check if a configuration hash is approved
    pub fn is_approved(&self, hash: &str) -> bool {
        self.approved_hashes.contains(hash)
    }

    /// Approve a configuration hash
    pub async fn approve(&mut self, hash: String) -> Result<()> {
        if self.approved_hashes.insert(hash.clone()) {
            info!("Approved configuration: {}", hash);
            self.save_approvals().await?;
        } else {
            debug!("Configuration already approved: {}", hash);
        }
        Ok(())
    }

    /// Revoke approval for a configuration hash
    pub async fn revoke(&mut self, hash: &str) -> Result<()> {
        if self.approved_hashes.remove(hash) {
            warn!("Revoked approval for configuration: {}", hash);
            self.save_approvals().await?;
        } else {
            debug!("Configuration was not approved: {}", hash);
        }
        Ok(())
    }

    /// List all approved hashes
    pub fn list_approved(&self) -> Vec<String> {
        self.approved_hashes.iter().cloned().collect()
    }

    /// Clear all approvals
    pub async fn clear_all(&mut self) -> Result<()> {
        let count = self.approved_hashes.len();
        self.approved_hashes.clear();
        warn!("Cleared {} approvals", count);
        self.save_approvals().await?;
        Ok(())
    }

    /// Compute the hash of a configuration value
    pub fn compute_hash(value: &serde_json::Value) -> String {
        let canonical = serde_json::to_string(value).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Compute hash from a configuration string
    pub fn compute_hash_from_string(config: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(config.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

/// Data structure for storing approvals
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApprovalData {
    /// Version of the approval file format
    version: u32,
    /// List of approved configuration hashes
    hashes: Vec<String>,
    /// Metadata about the approvals
    metadata: ApprovalMetadata,
}

/// Metadata for approvals
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApprovalMetadata {
    /// When the file was last updated
    last_updated: chrono::DateTime<chrono::Utc>,
}

/// Information about an approval request
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// The directory containing the configuration
    pub directory: PathBuf,
    /// Hash of the configuration
    pub config_hash: String,
    /// Whether the configuration is already approved
    pub is_approved: bool,
    /// Summary of what the configuration contains
    pub summary: ConfigSummary,
}

/// Summary of a configuration for approval display
#[derive(Debug, Clone)]
pub struct ConfigSummary {
    /// Number of environment variables
    pub env_vars: usize,
    /// Number of hooks
    pub hooks: usize,
    /// Number of tasks
    pub tasks: usize,
    /// Whether the config contains secrets
    pub has_secrets: bool,
}

impl ConfigSummary {
    /// Create an empty summary
    pub fn empty() -> Self {
        Self {
            env_vars: 0,
            hooks: 0,
            tasks: 0,
            has_secrets: false,
        }
    }

    /// Create a summary from a JSON value
    pub fn from_json(value: &serde_json::Value) -> Self {
        let mut summary = Self::empty();

        if let Some(obj) = value.as_object() {
            // Count environment variables
            if let Some(env) = obj.get("env").and_then(|v| v.as_object()) {
                summary.env_vars = env.len();
            }

            // Count hooks
            if let Some(hooks) = obj.get("hooks").and_then(|v| v.as_object()) {
                if let Some(on_enter) = hooks.get("onEnter") {
                    if let Some(arr) = on_enter.as_array() {
                        summary.hooks += arr.len();
                    } else if on_enter.is_object() {
                        summary.hooks += 1;
                    }
                }
                if let Some(on_exit) = hooks.get("onExit") {
                    if let Some(arr) = on_exit.as_array() {
                        summary.hooks += arr.len();
                    } else if on_exit.is_object() {
                        summary.hooks += 1;
                    }
                }
            }

            // Check for secrets
            if obj.contains_key("secrets") {
                summary.has_secrets = true;
            }
        }

        summary
    }

    /// Get a human-readable description
    pub fn description(&self) -> String {
        let mut parts = Vec::new();

        if self.env_vars > 0 {
            parts.push(format!("{} env vars", self.env_vars));
        }
        if self.hooks > 0 {
            parts.push(format!("{} hooks", self.hooks));
        }
        if self.tasks > 0 {
            parts.push(format!("{} tasks", self.tasks));
        }
        if self.has_secrets {
            parts.push("secrets".to_string());
        }

        if parts.is_empty() {
            "empty configuration".to_string()
        } else {
            parts.join(", ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_approval_manager_new() {
        let temp_dir = TempDir::new().unwrap();
        let approval_file = temp_dir.path().join("approved.json");
        let manager = ApprovalManager::new(approval_file.clone());

        assert_eq!(manager.approval_file, approval_file);
        assert!(manager.approved_hashes.is_empty());
    }

    #[tokio::test]
    async fn test_approve_and_check() {
        let temp_dir = TempDir::new().unwrap();
        let approval_file = temp_dir.path().join("approved.json");
        let mut manager = ApprovalManager::new(approval_file);

        let hash = "test_hash_123".to_string();

        assert!(!manager.is_approved(&hash));

        manager.approve(hash.clone()).await.unwrap();
        assert!(manager.is_approved(&hash));
    }

    #[tokio::test]
    async fn test_save_and_load_approvals() {
        let temp_dir = TempDir::new().unwrap();
        let approval_file = temp_dir.path().join("approved.json");

        // Create and save approvals
        {
            let mut manager = ApprovalManager::new(approval_file.clone());
            manager.approve("hash1".to_string()).await.unwrap();
            manager.approve("hash2".to_string()).await.unwrap();
        }

        // Load approvals in a new manager
        {
            let mut manager = ApprovalManager::new(approval_file);
            manager.load_approvals().await.unwrap();

            assert!(manager.is_approved("hash1"));
            assert!(manager.is_approved("hash2"));
            assert!(!manager.is_approved("hash3"));
        }
    }

    #[tokio::test]
    async fn test_revoke_approval() {
        let temp_dir = TempDir::new().unwrap();
        let approval_file = temp_dir.path().join("approved.json");
        let mut manager = ApprovalManager::new(approval_file);

        let hash = "revoke_test".to_string();

        manager.approve(hash.clone()).await.unwrap();
        assert!(manager.is_approved(&hash));

        manager.revoke(&hash).await.unwrap();
        assert!(!manager.is_approved(&hash));
    }

    #[tokio::test]
    async fn test_list_approved() {
        let temp_dir = TempDir::new().unwrap();
        let approval_file = temp_dir.path().join("approved.json");
        let mut manager = ApprovalManager::new(approval_file);

        manager.approve("hash_a".to_string()).await.unwrap();
        manager.approve("hash_b".to_string()).await.unwrap();
        manager.approve("hash_c".to_string()).await.unwrap();

        let mut approved = manager.list_approved();
        approved.sort();

        assert_eq!(approved, vec!["hash_a", "hash_b", "hash_c"]);
    }

    #[tokio::test]
    async fn test_clear_all() {
        let temp_dir = TempDir::new().unwrap();
        let approval_file = temp_dir.path().join("approved.json");
        let mut manager = ApprovalManager::new(approval_file);

        manager.approve("hash1".to_string()).await.unwrap();
        manager.approve("hash2".to_string()).await.unwrap();

        assert_eq!(manager.list_approved().len(), 2);

        manager.clear_all().await.unwrap();
        assert_eq!(manager.list_approved().len(), 0);
    }

    #[test]
    fn test_compute_hash() {
        let value = serde_json::json!({
            "env": {
                "TEST": "value"
            }
        });

        let hash1 = ApprovalManager::compute_hash(&value);
        let hash2 = ApprovalManager::compute_hash(&value);

        // Same value should produce same hash
        assert_eq!(hash1, hash2);

        // Different value should produce different hash
        let value2 = serde_json::json!({
            "env": {
                "TEST": "different"
            }
        });
        let hash3 = ApprovalManager::compute_hash(&value2);
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_compute_hash_from_string() {
        let config = "test configuration";
        let hash = ApprovalManager::compute_hash_from_string(config);

        // Should produce consistent hash
        let hash2 = ApprovalManager::compute_hash_from_string(config);
        assert_eq!(hash, hash2);

        // Different string should produce different hash
        let hash3 = ApprovalManager::compute_hash_from_string("different");
        assert_ne!(hash, hash3);
    }

    #[test]
    fn test_config_summary_from_json() {
        let value = serde_json::json!({
            "env": {
                "VAR1": "value1",
                "VAR2": "value2"
            },
            "hooks": {
                "onEnter": [
                    {"command": "echo", "args": ["hello"]},
                    {"command": "ls"}
                ],
                "onExit": {"command": "cleanup"}
            },
            "secrets": {
                "key": "value"
            }
        });

        let summary = ConfigSummary::from_json(&value);

        assert_eq!(summary.env_vars, 2);
        assert_eq!(summary.hooks, 3); // 2 onEnter + 1 onExit
        assert!(summary.has_secrets);
    }

    #[test]
    fn test_config_summary_description() {
        let summary = ConfigSummary {
            env_vars: 3,
            hooks: 2,
            tasks: 0,
            has_secrets: true,
        };

        let desc = summary.description();
        assert!(desc.contains("3 env vars"));
        assert!(desc.contains("2 hooks"));
        assert!(desc.contains("secrets"));
        assert!(!desc.contains("tasks")); // 0 tasks should not be mentioned
    }

    #[test]
    fn test_empty_config_summary() {
        let summary = ConfigSummary::empty();
        assert_eq!(summary.description(), "empty configuration");
    }
}
