//! Hash-based approval system for cuenv configurations
//!
//! This module provides security by requiring explicit approval of configuration
//! changes through hash-based validation. Users must run `cuenv allow` to approve
//! new or modified configurations before hooks can be executed.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

/// Approval record for a configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRecord {
    /// Hash of the approved configuration
    pub config_hash: String,
    /// Canonical path of the directory
    pub directory: PathBuf,
    /// When this configuration was approved
    pub approved_at: DateTime<Utc>,
    /// User who approved (system username)
    pub approved_by: String,
    /// Optional note about the approval
    pub note: Option<String>,
}

/// Manager for configuration approvals
#[derive(Debug)]
pub struct ApprovalManager {
    /// Path to the approvals file (~/.cuenv/approved.json)
    approvals_file: PathBuf,
}

impl ApprovalManager {
    /// Create a new approval manager
    /// 
    /// # Errors
    /// Returns an error if the cuenv directory cannot be created or accessed
    pub fn new() -> Result<Self> {
        let approvals_file = Self::get_approvals_file()?;
        
        // Ensure parent directory exists
        if let Some(parent) = approvals_file.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| Error::Io {
                    source: e,
                    path: Some(parent.into()),
                    operation: "create cuenv directory".to_string(),
                })?;
            }
        }

        Ok(Self { approvals_file })
    }

    /// Get the approvals file path (~/.cuenv/approved.json)
    fn get_approvals_file() -> Result<PathBuf> {
        let home_dir = dirs::home_dir()
            .ok_or_else(|| Error::configuration("Could not determine home directory"))?;
        
        Ok(home_dir.join(".cuenv").join("approved.json"))
    }

    /// Load approvals from disk
    fn load_approvals(&self) -> Result<HashMap<String, ApprovalRecord>> {
        if !self.approvals_file.exists() {
            return Ok(HashMap::new());
        }

        let content = fs::read_to_string(&self.approvals_file).map_err(|e| Error::Io {
            source: e,
            path: Some(self.approvals_file.clone().into()),
            operation: "read approvals file".to_string(),
        })?;

        let approvals: HashMap<String, ApprovalRecord> = 
            serde_json::from_str(&content).map_err(|e| {
                Error::configuration(format!("Failed to parse approvals file: {e}"))
            })?;

        Ok(approvals)
    }

    /// Save approvals to disk
    fn save_approvals(&self, approvals: &HashMap<String, ApprovalRecord>) -> Result<()> {
        let content = serde_json::to_string_pretty(approvals).map_err(|e| {
            Error::configuration(format!("Failed to serialize approvals: {e}"))
        })?;

        // Write atomically
        let temp_file = format!("{}.tmp", self.approvals_file.display());
        fs::write(&temp_file, content).map_err(|e| Error::Io {
            source: e,
            path: Some(PathBuf::from(temp_file.clone()).into()),
            operation: "write temporary approvals file".to_string(),
        })?;

        fs::rename(&temp_file, &self.approvals_file).map_err(|e| Error::Io {
            source: e,
            path: Some(self.approvals_file.clone().into()),
            operation: "atomic rename of approvals file".to_string(),
        })?;

        Ok(())
    }

    /// Generate a unique key for a directory (canonical path hash)
    fn directory_key(directory: &Path) -> Result<String> {
        let canonical = directory.canonicalize().map_err(|e| Error::Io {
            source: e,
            path: Some(directory.into()),
            operation: "canonicalize directory path".to_string(),
        })?;

        // Use hex encoding of SHA256 hash of canonical path
        let mut hasher = Sha256::new();
        hasher.update(canonical.to_string_lossy().as_bytes());
        Ok(format!("{:x}", hasher.finalize()))
    }

    /// Compute hash of a configuration value
    /// 
    /// This creates a deterministic hash of the configuration that can be used
    /// to detect changes requiring re-approval.
    pub fn compute_config_hash(config_value: &serde_json::Value) -> String {
        // Serialize to a canonical JSON representation
        let canonical_json = serde_json::to_string(config_value)
            .unwrap_or_else(|_| "{}".to_string());

        // Hash the canonical representation
        let mut hasher = Sha256::new();
        hasher.update(canonical_json.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Check if a configuration is approved for a directory
    /// 
    /// # Errors
    /// Returns an error if the approvals cannot be loaded or the directory path is invalid
    pub fn is_approved(&self, directory: &Path, config_hash: &str) -> Result<bool> {
        let directory_key = Self::directory_key(directory)?;
        let approvals = self.load_approvals()?;

        if let Some(record) = approvals.get(&directory_key) {
            Ok(record.config_hash == config_hash)
        } else {
            Ok(false)
        }
    }

    /// Approve a configuration for a directory
    /// 
    /// # Errors
    /// Returns an error if the approval cannot be saved or the directory path is invalid
    pub fn approve_config(
        &self,
        directory: &Path,
        config_hash: String,
        note: Option<String>,
    ) -> Result<()> {
        let directory_key = Self::directory_key(directory)?;
        let mut approvals = self.load_approvals()?;

        let current_user = whoami::username();
        let canonical_directory = directory.canonicalize().map_err(|e| Error::Io {
            source: e,
            path: Some(directory.into()),
            operation: "canonicalize directory for approval".to_string(),
        })?;

        let record = ApprovalRecord {
            config_hash,
            directory: canonical_directory,
            approved_at: Utc::now(),
            approved_by: current_user,
            note,
        };

        approvals.insert(directory_key, record);
        self.save_approvals(&approvals)?;

        Ok(())
    }

    /// Remove approval for a directory
    /// 
    /// # Errors
    /// Returns an error if the approval cannot be removed or the directory path is invalid
    pub fn revoke_approval(&self, directory: &Path) -> Result<bool> {
        let directory_key = Self::directory_key(directory)?;
        let mut approvals = self.load_approvals()?;

        let was_present = approvals.remove(&directory_key).is_some();
        if was_present {
            self.save_approvals(&approvals)?;
        }

        Ok(was_present)
    }

    /// Get approval record for a directory
    /// 
    /// # Errors
    /// Returns an error if the approvals cannot be loaded or the directory path is invalid
    pub fn get_approval(&self, directory: &Path) -> Result<Option<ApprovalRecord>> {
        let directory_key = Self::directory_key(directory)?;
        let approvals = self.load_approvals()?;
        Ok(approvals.get(&directory_key).cloned())
    }

    /// List all approved configurations
    /// 
    /// # Errors
    /// Returns an error if the approvals cannot be loaded
    pub fn list_approvals(&self) -> Result<Vec<ApprovalRecord>> {
        let approvals = self.load_approvals()?;
        Ok(approvals.into_values().collect())
    }

    /// Clean up approvals for directories that no longer exist
    /// 
    /// # Errors
    /// Returns an error if the approvals cannot be loaded or saved
    pub fn cleanup_stale_approvals(&self) -> Result<usize> {
        let mut approvals = self.load_approvals()?;
        let mut removed_count = 0;

        let mut to_remove = Vec::new();
        for (key, record) in &approvals {
            if !record.directory.exists() {
                to_remove.push(key.clone());
                removed_count += 1;
            }
        }

        for key in to_remove {
            approvals.remove(&key);
        }

        if removed_count > 0 {
            self.save_approvals(&approvals)?;
        }

        Ok(removed_count)
    }
}

/// Configuration approval status
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalStatus {
    /// Configuration is approved with the current hash
    Approved,
    /// Configuration has changed and needs re-approval
    RequiresApproval { current_hash: String },
    /// No approval record exists for this directory
    NotApproved { current_hash: String },
}

impl ApprovalStatus {
    /// Check if the configuration is approved
    #[must_use]
    pub fn is_approved(&self) -> bool {
        matches!(self, ApprovalStatus::Approved)
    }

    /// Get the current configuration hash
    #[must_use]
    pub fn current_hash(&self) -> Option<&str> {
        match self {
            ApprovalStatus::Approved => None,
            ApprovalStatus::RequiresApproval { current_hash } => Some(current_hash),
            ApprovalStatus::NotApproved { current_hash } => Some(current_hash),
        }
    }
}

impl std::fmt::Display for ApprovalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApprovalStatus::Approved => write!(f, "Configuration is approved"),
            ApprovalStatus::RequiresApproval { current_hash } => {
                write!(f, "Configuration has changed (hash: {}), run 'cuenv allow' to approve", 
                    &current_hash[..8])
            }
            ApprovalStatus::NotApproved { current_hash } => {
                write!(f, "Configuration not approved (hash: {}), run 'cuenv allow' to approve", 
                    &current_hash[..8])
            }
        }
    }
}

/// Check approval status for a configuration
/// 
/// # Errors
/// Returns an error if the approval status cannot be determined
pub fn check_approval_status(
    manager: &ApprovalManager,
    directory: &Path,
    config_value: &serde_json::Value,
) -> Result<ApprovalStatus> {
    let current_hash = ApprovalManager::compute_config_hash(config_value);
    
    if manager.is_approved(directory, &current_hash)? {
        Ok(ApprovalStatus::Approved)
    } else if let Some(_record) = manager.get_approval(directory)? {
        Ok(ApprovalStatus::RequiresApproval { current_hash })
    } else {
        Ok(ApprovalStatus::NotApproved { current_hash })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::env;

    fn create_test_approval_manager() -> (ApprovalManager, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        // Set HOME to temp dir for testing
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }
        let manager = ApprovalManager::new().unwrap();
        (manager, temp_dir)
    }

    #[test]
    fn test_approval_manager_new() {
        let (_manager, _temp) = create_test_approval_manager();
        // If we get here, ApprovalManager::new() succeeded
    }

    #[test]
    fn test_compute_config_hash() {
        let config1 = serde_json::json!({"key": "value1"});
        let config2 = serde_json::json!({"key": "value2"});
        let config1_dup = serde_json::json!({"key": "value1"});

        let hash1 = ApprovalManager::compute_config_hash(&config1);
        let hash2 = ApprovalManager::compute_config_hash(&config2);
        let hash1_dup = ApprovalManager::compute_config_hash(&config1_dup);

        // Same config should produce same hash
        assert_eq!(hash1, hash1_dup);
        
        // Different configs should produce different hashes
        assert_ne!(hash1, hash2);
        
        // Hash should be non-empty hex string
        assert!(!hash1.is_empty());
        assert!(hash1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_directory_key() {
        let temp_dir = TempDir::new().unwrap();
        let test_path = temp_dir.path();
        
        let key1 = ApprovalManager::directory_key(test_path).unwrap();
        let key2 = ApprovalManager::directory_key(test_path).unwrap();
        
        // Same path should generate same key
        assert_eq!(key1, key2);
        assert!(!key1.is_empty());
        // Key should be a hex string
        assert!(key1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_is_approved_not_approved() {
        let (manager, temp_dir) = create_test_approval_manager();
        let test_path = temp_dir.path();
        
        let is_approved = manager.is_approved(test_path, "test-hash").unwrap();
        assert!(!is_approved);
    }

    #[test]
    fn test_approve_and_check_config() {
        let (manager, temp_dir) = create_test_approval_manager();
        let test_path = temp_dir.path();
        let config_hash = "test-hash-123".to_string();
        
        // Initially not approved
        assert!(!manager.is_approved(test_path, &config_hash).unwrap());
        
        // Approve the config
        manager.approve_config(test_path, config_hash.clone(), Some("Test approval".to_string())).unwrap();
        
        // Now should be approved
        assert!(manager.is_approved(test_path, &config_hash).unwrap());
        
        // Different hash should not be approved
        assert!(!manager.is_approved(test_path, "different-hash").unwrap());
    }

    #[test]
    fn test_get_approval() {
        let (manager, temp_dir) = create_test_approval_manager();
        let test_path = temp_dir.path();
        let config_hash = "test-hash-456".to_string();
        
        // Initially no approval
        let approval = manager.get_approval(test_path).unwrap();
        assert!(approval.is_none());
        
        // Approve the config
        manager.approve_config(test_path, config_hash.clone(), Some("Test note".to_string())).unwrap();
        
        // Should have approval record
        let approval = manager.get_approval(test_path).unwrap();
        assert!(approval.is_some());
        
        let record = approval.unwrap();
        assert_eq!(record.config_hash, config_hash);
        assert_eq!(record.note, Some("Test note".to_string()));
        assert_eq!(record.approved_by, whoami::username());
    }

    #[test]
    fn test_revoke_approval() {
        let (manager, temp_dir) = create_test_approval_manager();
        let test_path = temp_dir.path();
        let config_hash = "test-hash-789".to_string();
        
        // Approve the config
        manager.approve_config(test_path, config_hash.clone(), None).unwrap();
        assert!(manager.is_approved(test_path, &config_hash).unwrap());
        
        // Revoke approval
        let was_present = manager.revoke_approval(test_path).unwrap();
        assert!(was_present);
        
        // Should no longer be approved
        assert!(!manager.is_approved(test_path, &config_hash).unwrap());
        
        // Revoking again should return false
        let was_present = manager.revoke_approval(test_path).unwrap();
        assert!(!was_present);
    }

    #[test]
    fn test_list_approvals() {
        let (manager, temp_dir) = create_test_approval_manager();
        
        // Initially empty
        let approvals = manager.list_approvals().unwrap();
        assert!(approvals.is_empty());
        
        // Add some approvals
        let test_path1 = temp_dir.path().join("dir1");
        fs::create_dir(&test_path1).unwrap();
        let test_path2 = temp_dir.path().join("dir2");
        fs::create_dir(&test_path2).unwrap();
        
        manager.approve_config(&test_path1, "hash1".to_string(), None).unwrap();
        manager.approve_config(&test_path2, "hash2".to_string(), Some("Note 2".to_string())).unwrap();
        
        // Should have both approvals
        let approvals = manager.list_approvals().unwrap();
        assert_eq!(approvals.len(), 2);
    }

    #[test]
    fn test_cleanup_stale_approvals() {
        let (manager, temp_dir) = create_test_approval_manager();
        
        // Create and approve a directory
        let test_path = temp_dir.path().join("test-dir");
        fs::create_dir(&test_path).unwrap();
        manager.approve_config(&test_path, "test-hash".to_string(), None).unwrap();
        
        // Verify approval exists
        assert!(manager.is_approved(&test_path, "test-hash").unwrap());
        
        // Remove the directory
        fs::remove_dir(&test_path).unwrap();
        
        // Cleanup stale approvals
        let removed_count = manager.cleanup_stale_approvals().unwrap();
        assert_eq!(removed_count, 1);
        
        // Approval should be gone (though this will fail due to directory not existing)
        let result = manager.is_approved(&test_path, "test-hash");
        assert!(result.is_err()); // Expected since directory doesn't exist
    }

    #[test]
    fn test_approval_status_display() {
        let approved = ApprovalStatus::Approved;
        assert_eq!(approved.to_string(), "Configuration is approved");
        
        let requires_approval = ApprovalStatus::RequiresApproval {
            current_hash: "abcdef1234567890".to_string(),
        };
        let display = requires_approval.to_string();
        assert!(display.contains("Configuration has changed"));
        assert!(display.contains("abcdef12")); // Truncated hash
        
        let not_approved = ApprovalStatus::NotApproved {
            current_hash: "1234567890abcdef".to_string(),
        };
        let display = not_approved.to_string();
        assert!(display.contains("Configuration not approved"));
        assert!(display.contains("12345678")); // Truncated hash
    }

    #[test]
    fn test_approval_status_methods() {
        let approved = ApprovalStatus::Approved;
        assert!(approved.is_approved());
        assert!(approved.current_hash().is_none());
        
        let hash = "test-hash".to_string();
        let not_approved = ApprovalStatus::NotApproved {
            current_hash: hash.clone(),
        };
        assert!(!not_approved.is_approved());
        assert_eq!(not_approved.current_hash(), Some("test-hash"));
    }

    #[test]
    fn test_check_approval_status() {
        let (manager, temp_dir) = create_test_approval_manager();
        let test_path = temp_dir.path();
        let config = serde_json::json!({"test": "value"});
        
        // Initially not approved
        let status = check_approval_status(&manager, test_path, &config).unwrap();
        assert!(!status.is_approved());
        assert!(matches!(status, ApprovalStatus::NotApproved { .. }));
        
        // Approve with correct hash
        let hash = ApprovalManager::compute_config_hash(&config);
        manager.approve_config(test_path, hash, None).unwrap();
        
        // Should be approved now
        let status = check_approval_status(&manager, test_path, &config).unwrap();
        assert!(status.is_approved());
        assert!(matches!(status, ApprovalStatus::Approved));
        
        // Different config should require re-approval
        let different_config = serde_json::json!({"test": "different_value"});
        let status = check_approval_status(&manager, test_path, &different_config).unwrap();
        assert!(!status.is_approved());
        assert!(matches!(status, ApprovalStatus::RequiresApproval { .. }));
    }

    #[test]
    fn test_approval_record_serialization() {
        let record = ApprovalRecord {
            config_hash: "test-hash".to_string(),
            directory: PathBuf::from("/test/path"),
            approved_at: Utc::now(),
            approved_by: "testuser".to_string(),
            note: Some("Test note".to_string()),
        };
        
        let serialized = serde_json::to_string(&record).unwrap();
        let deserialized: ApprovalRecord = serde_json::from_str(&serialized).unwrap();
        
        assert_eq!(record.config_hash, deserialized.config_hash);
        assert_eq!(record.directory, deserialized.directory);
        assert_eq!(record.approved_by, deserialized.approved_by);
        assert_eq!(record.note, deserialized.note);
    }
}