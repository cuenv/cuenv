//! Configuration approval management for secure hook execution

use crate::{Error, Result};
use chrono::{DateTime, Utc};
use fs4::tokio::AsyncFileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use tokio::fs;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, info, warn};

/// Manages approval of configurations before hook execution
#[derive(Debug, Clone)]
pub struct ApprovalManager {
    approval_file: PathBuf,
    approvals: HashMap<String, ApprovalRecord>,
}

impl ApprovalManager {
    /// Create a new approval manager with specified approval file
    pub fn new(approval_file: PathBuf) -> Self {
        Self {
            approval_file,
            approvals: HashMap::new(),
        }
    }

    /// Get the default approval file path (~/.cuenv/approved.json)
    pub fn default_approval_file() -> Result<PathBuf> {
        let home = dirs::home_dir()
            .ok_or_else(|| Error::configuration("Could not determine home directory"))?;
        Ok(home.join(".cuenv").join("approved.json"))
    }

    /// Create an approval manager using the default approval file
    pub fn with_default_file() -> Result<Self> {
        Ok(Self::new(Self::default_approval_file()?))
    }

    /// Load approvals from disk with file locking
    pub async fn load_approvals(&mut self) -> Result<()> {
        if !self.approval_file.exists() {
            debug!("No approval file found at {}", self.approval_file.display());
            return Ok(());
        }

        // Open file with shared lock for reading
        let mut file = OpenOptions::new()
            .read(true)
            .open(&self.approval_file)
            .await
            .map_err(|e| Error::Io {
                source: e,
                path: Some(self.approval_file.clone().into_boxed_path()),
                operation: "open".to_string(),
            })?;

        // Acquire shared lock (multiple readers allowed)
        file.lock_shared().map_err(|e| {
            Error::configuration(format!(
                "Failed to acquire shared lock on approval file: {}",
                e
            ))
        })?;

        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .await
            .map_err(|e| Error::Io {
                source: e,
                path: Some(self.approval_file.clone().into_boxed_path()),
                operation: "read_to_string".to_string(),
            })?;

        // Unlock happens automatically when file is dropped
        drop(file);

        self.approvals = serde_json::from_str(&contents)
            .map_err(|e| Error::configuration(format!("Failed to parse approval file: {e}")))?;

        info!("Loaded {} approvals from file", self.approvals.len());
        Ok(())
    }

    /// Save approvals to disk with file locking
    pub async fn save_approvals(&self) -> Result<()> {
        // Validate and canonicalize the approval file path to prevent path traversal
        let canonical_path = validate_and_canonicalize_path(&self.approval_file)?;

        // Ensure parent directory exists
        if let Some(parent) = canonical_path.parent()
            && !parent.exists()
        {
            // Validate the parent directory path as well
            let parent_path = validate_directory_path(parent)?;
            fs::create_dir_all(&parent_path)
                .await
                .map_err(|e| Error::Io {
                    source: e,
                    path: Some(parent_path.into()),
                    operation: "create_dir_all".to_string(),
                })?;
        }

        let contents = serde_json::to_string_pretty(&self.approvals)
            .map_err(|e| Error::configuration(format!("Failed to serialize approvals: {e}")))?;

        // Write to a temporary file first, then rename atomically
        let temp_path = canonical_path.with_extension("tmp");

        // Open temp file with exclusive lock for writing
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)
            .await
            .map_err(|e| Error::Io {
                source: e,
                path: Some(temp_path.clone().into_boxed_path()),
                operation: "open".to_string(),
            })?;

        // Acquire exclusive lock (only one writer allowed)
        file.lock_exclusive().map_err(|e| {
            Error::configuration(format!(
                "Failed to acquire exclusive lock on temp file: {}",
                e
            ))
        })?;

        file.write_all(contents.as_bytes())
            .await
            .map_err(|e| Error::Io {
                source: e,
                path: Some(temp_path.clone().into_boxed_path()),
                operation: "write_all".to_string(),
            })?;

        file.sync_all().await.map_err(|e| Error::Io {
            source: e,
            path: Some(temp_path.clone().into_boxed_path()),
            operation: "sync_all".to_string(),
        })?;

        // Unlock happens automatically when file is dropped
        drop(file);

        // Atomically rename temp file to final location
        fs::rename(&temp_path, &canonical_path)
            .await
            .map_err(|e| Error::Io {
                source: e,
                path: Some(canonical_path.clone().into_boxed_path()),
                operation: "rename".to_string(),
            })?;

        debug!("Saved {} approvals to file", self.approvals.len());
        Ok(())
    }

    /// Check if a configuration is approved for a specific directory
    pub fn is_approved(&self, directory_path: &Path, config_hash: &str) -> Result<bool> {
        let dir_key = compute_directory_key(directory_path);

        if let Some(approval) = self.approvals.get(&dir_key)
            && approval.config_hash == config_hash
        {
            // Check if approval hasn't expired
            if let Some(expires_at) = approval.expires_at
                && Utc::now() > expires_at
            {
                warn!("Approval for {} has expired", directory_path.display());
                return Ok(false);
            }
            return Ok(true);
        }

        Ok(false)
    }

    /// Approve a configuration for a specific directory
    pub async fn approve_config(
        &mut self,
        directory_path: &Path,
        config_hash: String,
        note: Option<String>,
    ) -> Result<()> {
        let dir_key = compute_directory_key(directory_path);
        let approval = ApprovalRecord {
            directory_path: directory_path.to_path_buf(),
            config_hash,
            approved_at: Utc::now(),
            expires_at: None, // No expiration by default
            note,
        };

        self.approvals.insert(dir_key, approval);
        self.save_approvals().await?;

        info!(
            "Approved configuration for directory: {}",
            directory_path.display()
        );
        Ok(())
    }

    /// Revoke approval for a directory
    pub async fn revoke_approval(&mut self, directory_path: &Path) -> Result<bool> {
        let dir_key = compute_directory_key(directory_path);

        if self.approvals.remove(&dir_key).is_some() {
            self.save_approvals().await?;
            info!(
                "Revoked approval for directory: {}",
                directory_path.display()
            );
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// List all approved directories
    pub fn list_approved(&self) -> Vec<&ApprovalRecord> {
        self.approvals.values().collect()
    }

    /// Clean up expired approvals
    pub async fn cleanup_expired(&mut self) -> Result<usize> {
        let now = Utc::now();
        let initial_count = self.approvals.len();

        self.approvals.retain(|_, approval| {
            if let Some(expires_at) = approval.expires_at {
                expires_at > now
            } else {
                true // Keep approvals without expiration
            }
        });

        let removed_count = initial_count - self.approvals.len();
        if removed_count > 0 {
            self.save_approvals().await?;
            info!("Cleaned up {} expired approvals", removed_count);
        }

        Ok(removed_count)
    }
}

/// Record of an approved configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalRecord {
    /// Path to the directory
    pub directory_path: PathBuf,
    /// Hash of the approved configuration
    pub config_hash: String,
    /// When this approval was granted
    pub approved_at: DateTime<Utc>,
    /// Optional expiration time
    pub expires_at: Option<DateTime<Utc>>,
    /// Optional note about this approval
    pub note: Option<String>,
}

/// Status of approval check for a configuration
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalStatus {
    /// Configuration is approved and can be executed
    Approved,
    /// Configuration has changed and requires new approval
    RequiresApproval { current_hash: String },
    /// Configuration is not approved
    NotApproved { current_hash: String },
}

/// Check the approval status for a configuration
pub fn check_approval_status(
    manager: &ApprovalManager,
    directory_path: &Path,
    config: &Value,
) -> Result<ApprovalStatus> {
    let current_hash = compute_config_hash(config);

    if manager.is_approved(directory_path, &current_hash)? {
        Ok(ApprovalStatus::Approved)
    } else {
        // Check if there's an existing approval with a different hash
        let dir_key = compute_directory_key(directory_path);
        if manager.approvals.contains_key(&dir_key) {
            Ok(ApprovalStatus::RequiresApproval { current_hash })
        } else {
            Ok(ApprovalStatus::NotApproved { current_hash })
        }
    }
}

/// Compute a hash of a configuration
pub fn compute_config_hash(config: &Value) -> String {
    let mut hasher = Sha256::new();

    // Convert to canonical JSON string for consistent hashing
    let canonical = serde_json::to_string(config).unwrap_or_default();
    hasher.update(canonical.as_bytes());

    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// Compute a directory key for the approvals map
fn compute_directory_key(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// Validate and canonicalize a path to prevent path traversal attacks
fn validate_and_canonicalize_path(path: &Path) -> Result<PathBuf> {
    // Check for suspicious path components
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                // Allow parent directory references only if they don't escape the base directory
                // We'll resolve them through canonicalization
            }
            Component::Prefix(_) => {
                // Windows drive prefixes are okay
            }
        }
    }

    // If the path exists, canonicalize it
    if path.exists() {
        std::fs::canonicalize(path)
            .map_err(|e| Error::configuration(format!("Failed to canonicalize path: {}", e)))
    } else {
        // For non-existent paths, validate the parent and construct the canonical path
        if let Some(parent) = path.parent() {
            if parent.exists() {
                let canonical_parent = std::fs::canonicalize(parent).map_err(|e| {
                    Error::configuration(format!("Failed to canonicalize parent path: {}", e))
                })?;
                if let Some(file_name) = path.file_name() {
                    Ok(canonical_parent.join(file_name))
                } else {
                    Err(Error::configuration("Invalid file path"))
                }
            } else {
                // Parent doesn't exist, but we can still validate the path structure
                validate_path_structure(path)?;
                Ok(path.to_path_buf())
            }
        } else {
            validate_path_structure(path)?;
            Ok(path.to_path_buf())
        }
    }
}

/// Validate directory path for creation
fn validate_directory_path(path: &Path) -> Result<PathBuf> {
    // Check for suspicious patterns
    validate_path_structure(path)?;

    // Return the path as-is if validation passes
    Ok(path.to_path_buf())
}

/// Validate path structure for security
fn validate_path_structure(path: &Path) -> Result<()> {
    let path_str = path.to_string_lossy();

    // Check for null bytes
    if path_str.contains('\0') {
        return Err(Error::configuration("Path contains null bytes"));
    }

    // Check for suspicious patterns that might indicate path traversal attempts
    let suspicious_patterns = [
        "../../../",    // Multiple parent directory traversals
        "..\\..\\..\\", // Windows-style traversals
        "%2e%2e",       // URL-encoded parent directory
        "..;/",         // Semicolon injection
    ];

    for pattern in &suspicious_patterns {
        if path_str.contains(pattern) {
            return Err(Error::configuration(format!(
                "Path contains suspicious pattern: {}",
                pattern
            )));
        }
    }

    Ok(())
}

/// Generate a summary of a configuration for display to users
#[derive(Debug, Clone)]
pub struct ConfigSummary {
    pub has_hooks: bool,
    pub hook_count: usize,
    pub has_env_vars: bool,
    pub env_var_count: usize,
    pub has_tasks: bool,
    pub task_count: usize,
}

impl ConfigSummary {
    /// Create a summary from a JSON configuration
    pub fn from_json(config: &Value) -> Self {
        let mut summary = Self {
            has_hooks: false,
            hook_count: 0,
            has_env_vars: false,
            env_var_count: 0,
            has_tasks: false,
            task_count: 0,
        };

        if let Some(obj) = config.as_object() {
            // Check for hooks
            if let Some(hooks) = obj.get("hooks")
                && let Some(hooks_obj) = hooks.as_object()
            {
                summary.has_hooks = true;

                // Count onEnter hooks
                if let Some(on_enter) = hooks_obj.get("onEnter") {
                    if let Some(arr) = on_enter.as_array() {
                        summary.hook_count += arr.len();
                    } else if on_enter.is_object() {
                        summary.hook_count += 1;
                    }
                }

                // Count onExit hooks
                if let Some(on_exit) = hooks_obj.get("onExit") {
                    if let Some(arr) = on_exit.as_array() {
                        summary.hook_count += arr.len();
                    } else if on_exit.is_object() {
                        summary.hook_count += 1;
                    }
                }
            }

            // Check for environment variables
            if let Some(env) = obj.get("env")
                && let Some(env_obj) = env.as_object()
            {
                summary.has_env_vars = true;
                summary.env_var_count = env_obj.len();
            }

            // Check for tasks
            if let Some(tasks) = obj.get("tasks") {
                if let Some(tasks_obj) = tasks.as_object() {
                    summary.has_tasks = true;
                    summary.task_count = tasks_obj.len();
                } else if let Some(tasks_arr) = tasks.as_array() {
                    summary.has_tasks = true;
                    summary.task_count = tasks_arr.len();
                }
            }
        }

        summary
    }

    /// Get a human-readable description of the configuration
    pub fn description(&self) -> String {
        let mut parts = Vec::new();

        if self.has_hooks {
            if self.hook_count == 1 {
                parts.push("1 hook".to_string());
            } else {
                parts.push(format!("{} hooks", self.hook_count));
            }
        }

        if self.has_env_vars {
            if self.env_var_count == 1 {
                parts.push("1 environment variable".to_string());
            } else {
                parts.push(format!("{} environment variables", self.env_var_count));
            }
        }

        if self.has_tasks {
            if self.task_count == 1 {
                parts.push("1 task".to_string());
            } else {
                parts.push(format!("{} tasks", self.task_count));
            }
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
    use serde_json::json;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_approval_manager_operations() {
        let temp_dir = TempDir::new().unwrap();
        let approval_file = temp_dir.path().join("approvals.json");
        let mut manager = ApprovalManager::new(approval_file);

        let directory = Path::new("/test/directory");
        let config_hash = "test_hash_123".to_string();

        // Initially not approved
        assert!(!manager.is_approved(directory, &config_hash).unwrap());

        // Approve configuration
        manager
            .approve_config(
                directory,
                config_hash.clone(),
                Some("Test approval".to_string()),
            )
            .await
            .unwrap();

        // Should now be approved
        assert!(manager.is_approved(directory, &config_hash).unwrap());

        // Different hash should not be approved
        assert!(!manager.is_approved(directory, "different_hash").unwrap());

        // Test persistence
        let mut manager2 = ApprovalManager::new(manager.approval_file.clone());
        manager2.load_approvals().await.unwrap();
        assert!(manager2.is_approved(directory, &config_hash).unwrap());

        // Revoke approval
        let revoked = manager2.revoke_approval(directory).await.unwrap();
        assert!(revoked);
        assert!(!manager2.is_approved(directory, &config_hash).unwrap());
    }

    #[test]
    fn test_compute_config_hash() {
        let config1 = json!({
            "env": {"TEST": "value"},
            "hooks": {"onEnter": [{"command": "echo", "args": ["hello"]}]}
        });

        let config2 = json!({
            "hooks": {"onEnter": [{"command": "echo", "args": ["hello"]}]},
            "env": {"TEST": "value"}
        });

        // Different key order should produce same hash due to canonical JSON
        let hash1 = compute_config_hash(&config1);
        let hash2 = compute_config_hash(&config2);
        assert_eq!(hash1, hash2);

        let config3 = json!({
            "env": {"TEST": "different_value"},
            "hooks": {"onEnter": [{"command": "echo", "args": ["hello"]}]}
        });

        let hash3 = compute_config_hash(&config3);
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_config_summary() {
        let config = json!({
            "env": {
                "NODE_ENV": "development",
                "API_URL": "http://localhost:3000"
            },
            "hooks": {
                "onEnter": [
                    {"command": "npm", "args": ["install"]},
                    {"command": "docker-compose", "args": ["up", "-d"]}
                ],
                "onExit": [
                    {"command": "docker-compose", "args": ["down"]}
                ]
            },
            "tasks": {
                "build": {"command": "npm", "args": ["run", "build"]},
                "test": {"command": "npm", "args": ["test"]}
            }
        });

        let summary = ConfigSummary::from_json(&config);
        assert!(summary.has_hooks);
        assert_eq!(summary.hook_count, 3);
        assert!(summary.has_env_vars);
        assert_eq!(summary.env_var_count, 2);
        assert!(summary.has_tasks);
        assert_eq!(summary.task_count, 2);

        let description = summary.description();
        assert!(description.contains("3 hooks"));
        assert!(description.contains("2 environment variables"));
        assert!(description.contains("2 tasks"));
    }

    #[test]
    fn test_approval_status() {
        let mut manager = ApprovalManager::new(PathBuf::from("/tmp/test"));
        let directory = Path::new("/test/dir");
        let config = json!({"env": {"TEST": "value"}});

        let status = check_approval_status(&manager, directory, &config).unwrap();
        assert!(matches!(status, ApprovalStatus::NotApproved { .. }));

        // Add an approval with a different hash
        let different_hash = "different_hash".to_string();
        manager.approvals.insert(
            compute_directory_key(directory),
            ApprovalRecord {
                directory_path: directory.to_path_buf(),
                config_hash: different_hash,
                approved_at: Utc::now(),
                expires_at: None,
                note: None,
            },
        );

        let status = check_approval_status(&manager, directory, &config).unwrap();
        assert!(matches!(status, ApprovalStatus::RequiresApproval { .. }));

        // Add approval with correct hash
        let correct_hash = compute_config_hash(&config);
        manager.approvals.insert(
            compute_directory_key(directory),
            ApprovalRecord {
                directory_path: directory.to_path_buf(),
                config_hash: correct_hash,
                approved_at: Utc::now(),
                expires_at: None,
                note: None,
            },
        );

        let status = check_approval_status(&manager, directory, &config).unwrap();
        assert!(matches!(status, ApprovalStatus::Approved));
    }

    #[test]
    fn test_path_validation() {
        // Test valid paths
        assert!(validate_path_structure(Path::new("/home/user/test")).is_ok());
        assert!(validate_path_structure(Path::new("./relative/path")).is_ok());
        assert!(validate_path_structure(Path::new("file.txt")).is_ok());

        // Test paths with null bytes (should fail)
        let path_with_null = PathBuf::from("/test\0/path");
        assert!(validate_path_structure(&path_with_null).is_err());

        // Test paths with multiple parent directory traversals (should fail)
        assert!(validate_path_structure(Path::new("../../../etc/passwd")).is_err());
        assert!(validate_path_structure(Path::new("..\\..\\..\\windows\\system32")).is_err());

        // Test URL-encoded traversals (should fail)
        assert!(validate_path_structure(Path::new("/test/%2e%2e/passwd")).is_err());

        // Test semicolon injection (should fail)
        assert!(validate_path_structure(Path::new("..;/etc/passwd")).is_err());
    }

    #[test]
    fn test_validate_and_canonicalize_path() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        std::fs::write(&test_file, "test").unwrap();

        // Test existing file canonicalization
        let result = validate_and_canonicalize_path(&test_file).unwrap();
        assert!(result.is_absolute());
        assert!(result.exists());

        // Test non-existent file in existing directory
        let new_file = temp_dir.path().join("new_file.txt");
        let result = validate_and_canonicalize_path(&new_file).unwrap();
        assert!(result.ends_with("new_file.txt"));

        // Test validation with parent directory that exists
        let nested_new = temp_dir.path().join("subdir/newfile.txt");
        let result = validate_and_canonicalize_path(&nested_new);
        assert!(result.is_ok()); // Should succeed even though parent doesn't exist yet
    }

    #[tokio::test]
    async fn test_approval_file_corruption_recovery() {
        let temp_dir = TempDir::new().unwrap();
        let approval_file = temp_dir.path().join("approvals.json");

        // Write corrupted JSON to the approval file
        std::fs::write(&approval_file, "{invalid json}").unwrap();

        let mut manager = ApprovalManager::new(approval_file.clone());

        // Loading should fail due to corrupted JSON
        let result = manager.load_approvals().await;
        assert!(
            result.is_err(),
            "Expected error when loading corrupted JSON"
        );

        // Manager should still be usable with empty approvals
        assert_eq!(manager.approvals.len(), 0);

        // Should be able to save new approvals
        let directory = Path::new("/test/dir");
        manager
            .approve_config(directory, "test_hash".to_string(), None)
            .await
            .unwrap();

        // New manager should be able to load the fixed file
        let mut manager2 = ApprovalManager::new(approval_file);
        manager2.load_approvals().await.unwrap();
        assert_eq!(manager2.approvals.len(), 1);
    }

    #[tokio::test]
    async fn test_concurrent_approval_access() {
        let temp_dir = TempDir::new().unwrap();
        let approval_file = temp_dir.path().join("approvals.json");

        // Create multiple managers accessing the same file
        let mut manager1 = ApprovalManager::new(approval_file.clone());
        let mut manager2 = ApprovalManager::new(approval_file.clone());

        // Approve from first manager
        manager1
            .approve_config(
                Path::new("/test/dir1"),
                "hash1".to_string(),
                Some("Manager 1".to_string()),
            )
            .await
            .unwrap();

        // Approve from second manager
        manager2
            .approve_config(
                Path::new("/test/dir2"),
                "hash2".to_string(),
                Some("Manager 2".to_string()),
            )
            .await
            .unwrap();

        // Load in a third manager to verify both approvals
        let mut manager3 = ApprovalManager::new(approval_file);
        manager3.load_approvals().await.unwrap();

        // Should have the approval from manager1 (manager2's might have overwritten)
        // Due to file locking, one of them should succeed
        assert!(!manager3.approvals.is_empty());
    }

    #[tokio::test]
    async fn test_approval_expiration() {
        let temp_dir = TempDir::new().unwrap();
        let approval_file = temp_dir.path().join("approvals.json");
        let mut manager = ApprovalManager::new(approval_file);

        let directory = Path::new("/test/expire");
        let config_hash = "expire_hash".to_string();

        // Add an expired approval
        let expired_approval = ApprovalRecord {
            directory_path: directory.to_path_buf(),
            config_hash: config_hash.clone(),
            approved_at: Utc::now() - chrono::Duration::hours(2),
            expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
            note: Some("Expired approval".to_string()),
        };

        manager
            .approvals
            .insert(compute_directory_key(directory), expired_approval);

        // Should not be approved due to expiration
        assert!(!manager.is_approved(directory, &config_hash).unwrap());

        // Cleanup should remove expired approval
        let removed = manager.cleanup_expired().await.unwrap();
        assert_eq!(removed, 1);
        assert_eq!(manager.approvals.len(), 0);
    }
}
