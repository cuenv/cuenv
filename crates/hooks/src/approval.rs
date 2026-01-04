//! Configuration approval management for secure hook execution

use crate::types::{Hook, Hooks};
use crate::{Error, Result};
use chrono::{DateTime, Utc};
use fs4::tokio::AsyncFileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use tokio::fs;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, info, warn};

/// CI provider environment variables to check when detecting CI environments
const CI_VARS: &[&str] = &[
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "BUILDKITE",
    "JENKINS_URL",
    "CIRCLECI",
    "TRAVIS",
    "BITBUCKET_PIPELINES",
    "AZURE_PIPELINES",
    "TF_BUILD",
    "DRONE",
    "TEAMCITY_VERSION",
];

/// Check if the current process is running in a CI environment.
///
/// This checks for common CI environment variables used by popular CI/CD systems:
/// - `CI` - Generic CI indicator (GitHub Actions, GitLab CI, CircleCI, Travis CI, etc.)
/// - `GITHUB_ACTIONS` - GitHub Actions
/// - `GITLAB_CI` - GitLab CI
/// - `BUILDKITE` - Buildkite
/// - `JENKINS_URL` - Jenkins
/// - `CIRCLECI` - CircleCI
/// - `TRAVIS` - Travis CI
/// - `BITBUCKET_PIPELINES` - Bitbucket Pipelines
/// - `AZURE_PIPELINES` - Azure Pipelines
/// - `TF_BUILD` - Azure DevOps / Team Foundation Build
/// - `DRONE` - Drone CI
/// - `TEAMCITY_VERSION` - TeamCity
///
/// Returns `true` if any of these environment variables are set to a truthy value.
#[must_use]
pub fn is_ci() -> bool {
    // Check for the generic CI variable first (most CI systems set this)
    if std::env::var("CI")
        .map(|v| !v.is_empty() && v != "0" && v.to_lowercase() != "false")
        .unwrap_or(false)
    {
        return true;
    }

    // Check for specific CI provider variables
    CI_VARS.iter().any(|var| std::env::var(var).is_ok())
}

/// Manages approval of configurations before hook execution
#[derive(Debug, Clone)]
pub struct ApprovalManager {
    approval_file: PathBuf,
    approvals: HashMap<String, ApprovalRecord>,
}

impl ApprovalManager {
    /// Create a new approval manager with specified approval file
    #[must_use]
    pub fn new(approval_file: PathBuf) -> Self {
        Self {
            approval_file,
            approvals: HashMap::new(),
        }
    }

    /// Get the default approval file path.
    ///
    /// Uses platform-appropriate paths:
    /// - Linux: `~/.local/state/cuenv/approved.json`
    /// - macOS: `~/Library/Application Support/cuenv/approved.json`
    /// - Windows: `%APPDATA%\cuenv\approved.json`
    ///
    /// Can be overridden with `CUENV_APPROVAL_FILE` environment variable.
    pub fn default_approval_file() -> Result<PathBuf> {
        // Check for CUENV_APPROVAL_FILE environment variable first
        if let Ok(approval_file) = std::env::var("CUENV_APPROVAL_FILE")
            && !approval_file.is_empty()
        {
            return Ok(PathBuf::from(approval_file));
        }

        // Use platform-appropriate paths via dirs crate
        let base = dirs::state_dir()
            .or_else(dirs::data_dir)
            .ok_or_else(|| Error::configuration("Could not determine state directory"))?;

        Ok(base.join("cuenv").join("approved.json"))
    }

    /// Create an approval manager using the default approval file
    pub fn with_default_file() -> Result<Self> {
        Ok(Self::new(Self::default_approval_file()?))
    }

    /// Get approval for a specific directory
    #[must_use]
    pub fn get_approval(&self, directory: &str) -> Option<&ApprovalRecord> {
        let path = PathBuf::from(directory);
        let dir_key = compute_directory_key(&path);
        self.approvals.get(&dir_key)
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
            .map_err(|e| Error::serialization(format!("Failed to parse approval file: {e}")))?;

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
            .map_err(|e| Error::serialization(format!("Failed to serialize approvals: {e}")))?;

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
    #[must_use]
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

    /// Check if the approvals map contains a specific directory key
    #[must_use]
    pub fn contains_key(&self, directory_path: &Path) -> bool {
        let dir_key = compute_directory_key(directory_path);
        self.approvals.contains_key(&dir_key)
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
    RequiresApproval {
        /// The hash of the current configuration that needs approval
        current_hash: String,
    },
    /// Configuration is not approved
    NotApproved {
        /// The hash of the current configuration that is not approved
        current_hash: String,
    },
}

#[derive(Debug, Serialize)]
struct ApprovalHashInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    hooks: Option<HooksForHash>,
}

#[derive(Debug, Serialize)]
struct HooksForHash {
    #[serde(skip_serializing_if = "Option::is_none", rename = "onEnter")]
    on_enter: Option<BTreeMap<String, Hook>>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "onExit")]
    on_exit: Option<BTreeMap<String, Hook>>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "prePush")]
    pre_push: Option<BTreeMap<String, Hook>>,
}

impl HooksForHash {
    fn from_hooks(hooks: &Hooks) -> Self {
        Self {
            on_enter: hooks.on_enter.as_ref().map(sorted_hooks_map),
            on_exit: hooks.on_exit.as_ref().map(sorted_hooks_map),
            pre_push: hooks.pre_push.as_ref().map(sorted_hooks_map),
        }
    }
}

fn sorted_hooks_map(map: &HashMap<String, Hook>) -> BTreeMap<String, Hook> {
    map.iter()
        .map(|(name, hook)| (name.clone(), hook.clone()))
        .collect()
}

/// Check the approval status for a configuration.
///
/// In CI environments (detected via [`is_ci`]), hooks are always auto-approved
/// since CI environments are typically non-interactive and already secured.
pub fn check_approval_status(
    manager: &ApprovalManager,
    directory_path: &Path,
    hooks: Option<&Hooks>,
) -> Result<ApprovalStatus> {
    // Auto-approve in CI environments - they are non-interactive and already secured
    if is_ci() {
        debug!(
            "Auto-approving hooks in CI environment for {}",
            directory_path.display()
        );
        return Ok(ApprovalStatus::Approved);
    }

    check_approval_status_core(manager, directory_path, hooks)
}

/// Core approval logic without CI bypass.
///
/// This function contains the actual approval checking logic and is used by tests
/// to verify behavior without CI environment interference.
fn check_approval_status_core(
    manager: &ApprovalManager,
    directory_path: &Path,
    hooks: Option<&Hooks>,
) -> Result<ApprovalStatus> {
    let current_hash = compute_approval_hash(hooks);

    if manager.is_approved(directory_path, &current_hash)? {
        Ok(ApprovalStatus::Approved)
    } else {
        // Check if there's an existing approval with a different hash
        if manager.contains_key(directory_path) {
            Ok(ApprovalStatus::RequiresApproval { current_hash })
        } else {
            Ok(ApprovalStatus::NotApproved { current_hash })
        }
    }
}

/// Compute a hash for approval based only on security-sensitive hooks.
///
/// Only onEnter, onExit, and prePush hooks are included since they execute arbitrary commands.
/// Changes to env vars, tasks, config settings do NOT require re-approval.
#[must_use]
pub fn compute_approval_hash(hooks: Option<&Hooks>) -> String {
    let mut hasher = Sha256::new();

    // Extract only the hooks portion for hashing
    // Treat empty hooks the same as no hooks for consistent hashing
    let hooks_for_hash = hooks.and_then(|h| {
        let hfh = HooksForHash::from_hooks(h);
        // If all fields are None, treat as no hooks
        if hfh.on_enter.is_none() && hfh.on_exit.is_none() && hfh.pre_push.is_none() {
            None
        } else {
            Some(hfh)
        }
    });
    let hooks_only = ApprovalHashInput {
        hooks: hooks_for_hash,
    };
    let canonical = serde_json::to_string(&hooks_only).unwrap_or_default();
    hasher.update(canonical.as_bytes());

    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// Compute a directory key for the approvals map
#[must_use]
pub fn compute_directory_key(path: &Path) -> String {
    // Try to canonicalize the path for consistency
    // If canonicalization fails (e.g., path doesn't exist), use the path as-is
    let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    let mut hasher = Sha256::new();
    hasher.update(canonical_path.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// Validate and canonicalize a path to prevent path traversal attacks
fn validate_and_canonicalize_path(path: &Path) -> Result<PathBuf> {
    // All path components are allowed - parent directory references and prefixes
    // are resolved through canonicalization, and we validate the final result
    for component in path.components() {
        match component {
            // All component types are valid for input paths:
            // - Normal: regular path segments
            // - RootDir/CurDir: explicit root or current directory
            // - ParentDir: resolved through canonicalization
            // - Prefix: Windows drive prefixes
            Component::Normal(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir
            | Component::Prefix(_) => {}
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

/// Summary of hook counts for display
#[derive(Debug, Clone)]
pub struct ConfigSummary {
    /// Whether any hooks are defined
    pub has_hooks: bool,
    /// Total number of hooks across all hook types
    pub hook_count: usize,
}

impl ConfigSummary {
    /// Create a summary from hooks
    #[must_use]
    pub fn from_hooks(hooks: Option<&Hooks>) -> Self {
        let mut summary = Self {
            has_hooks: false,
            hook_count: 0,
        };

        if let Some(hooks) = hooks {
            let on_enter_count = hooks.on_enter.as_ref().map_or(0, |map| map.len());
            let on_exit_count = hooks.on_exit.as_ref().map_or(0, |map| map.len());
            let pre_push_count = hooks.pre_push.as_ref().map_or(0, |map| map.len());
            summary.hook_count = on_enter_count + on_exit_count + pre_push_count;
            summary.has_hooks = summary.hook_count > 0;
        }

        summary
    }

    /// Get a human-readable description of the hooks
    #[must_use]
    pub fn description(&self) -> String {
        if !self.has_hooks {
            "no hooks".to_string()
        } else if self.hook_count == 1 {
            "1 hook".to_string()
        } else {
            format!("{} hooks", self.hook_count)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_hook(command: &str, args: &[&str]) -> Hook {
        Hook {
            order: 100,
            propagate: false,
            command: command.to_string(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            dir: None,
            inputs: vec![],
            source: None,
        }
    }

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
    fn test_approval_hash_consistency() {
        // Same hooks should produce same hash
        let mut hooks_map = HashMap::new();
        hooks_map.insert("setup".to_string(), make_hook("echo", &["hello"]));
        let hooks = Hooks {
            on_enter: Some(hooks_map.clone()),
            on_exit: None,
            pre_push: None,
        };

        let hash1 = compute_approval_hash(Some(&hooks));
        let hash2 = compute_approval_hash(Some(&hooks));
        assert_eq!(hash1, hash2, "Same hooks should produce same hash");

        // Different hooks should produce different hash
        let mut hooks_map2 = HashMap::new();
        hooks_map2.insert("setup".to_string(), make_hook("echo", &["world"]));
        let hooks2 = Hooks {
            on_enter: Some(hooks_map2),
            on_exit: None,
            pre_push: None,
        };

        let hash3 = compute_approval_hash(Some(&hooks2));
        assert_ne!(
            hash1, hash3,
            "Different hooks should produce different hash"
        );
    }

    #[test]
    fn test_approval_hash_no_hooks() {
        // Configs without hooks should produce consistent hash
        let hash1 = compute_approval_hash(None);
        let hash2 = compute_approval_hash(None);
        assert_eq!(hash1, hash2, "No hooks should produce consistent hash");

        // Empty hooks should be same as no hooks
        let empty_hooks = Hooks {
            on_enter: None,
            on_exit: None,
            pre_push: None,
        };
        let hash3 = compute_approval_hash(Some(&empty_hooks));
        assert_eq!(hash1, hash3, "Empty hooks should be same as no hooks");
    }

    #[test]
    fn test_config_summary() {
        let mut on_enter = HashMap::new();
        on_enter.insert("npm".to_string(), make_hook("npm", &["install"]));
        on_enter.insert(
            "docker".to_string(),
            make_hook("docker-compose", &["up", "-d"]),
        );

        let mut on_exit = HashMap::new();
        on_exit.insert("docker".to_string(), make_hook("docker-compose", &["down"]));

        let hooks = Hooks {
            on_enter: Some(on_enter),
            on_exit: Some(on_exit),
            pre_push: None,
        };

        let summary = ConfigSummary::from_hooks(Some(&hooks));
        assert!(summary.has_hooks);
        assert_eq!(summary.hook_count, 3);

        let description = summary.description();
        assert!(description.contains("3 hooks"));
    }

    #[test]
    fn test_approval_status() {
        let mut manager = ApprovalManager::new(PathBuf::from("/tmp/test"));
        let directory = Path::new("/test/dir");
        let hooks = Hooks {
            on_enter: None,
            on_exit: None,
            pre_push: None,
        };

        let status = check_approval_status_core(&manager, directory, Some(&hooks)).unwrap();
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

        let status = check_approval_status_core(&manager, directory, Some(&hooks)).unwrap();
        assert!(matches!(status, ApprovalStatus::RequiresApproval { .. }));

        // Add approval with correct hash
        let correct_hash = compute_approval_hash(Some(&hooks));
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

        let status = check_approval_status_core(&manager, directory, Some(&hooks)).unwrap();
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

    #[test]
    fn test_is_ci_with_ci_env_var() {
        // Test with CI=true
        temp_env::with_var("CI", Some("true"), || {
            assert!(is_ci());
        });

        // Test with CI=1
        temp_env::with_var("CI", Some("1"), || {
            assert!(is_ci());
        });

        // Test with CI=yes (any non-empty, non-false value)
        temp_env::with_var("CI", Some("yes"), || {
            assert!(is_ci());
        });

        // Test with CI=false (should NOT be detected as CI)
        temp_env::with_var("CI", Some("false"), || {
            // Clear other CI vars to isolate the test
            temp_env::with_vars_unset(
                vec![
                    "GITHUB_ACTIONS",
                    "GITLAB_CI",
                    "BUILDKITE",
                    "JENKINS_URL",
                    "CIRCLECI",
                    "TRAVIS",
                    "BITBUCKET_PIPELINES",
                    "AZURE_PIPELINES",
                    "TF_BUILD",
                    "DRONE",
                    "TEAMCITY_VERSION",
                ],
                || {
                    assert!(!is_ci());
                },
            );
        });

        // Test with CI=0 (should NOT be detected as CI)
        temp_env::with_var("CI", Some("0"), || {
            temp_env::with_vars_unset(
                vec![
                    "GITHUB_ACTIONS",
                    "GITLAB_CI",
                    "BUILDKITE",
                    "JENKINS_URL",
                    "CIRCLECI",
                    "TRAVIS",
                    "BITBUCKET_PIPELINES",
                    "AZURE_PIPELINES",
                    "TF_BUILD",
                    "DRONE",
                    "TEAMCITY_VERSION",
                ],
                || {
                    assert!(!is_ci());
                },
            );
        });
    }

    #[test]
    fn test_is_ci_with_provider_specific_vars() {
        // Test GitHub Actions
        temp_env::with_var_unset("CI", || {
            temp_env::with_var("GITHUB_ACTIONS", Some("true"), || {
                assert!(is_ci());
            });
        });

        // Test GitLab CI
        temp_env::with_var_unset("CI", || {
            temp_env::with_var("GITLAB_CI", Some("true"), || {
                assert!(is_ci());
            });
        });

        // Test Buildkite
        temp_env::with_var_unset("CI", || {
            temp_env::with_var("BUILDKITE", Some("true"), || {
                assert!(is_ci());
            });
        });

        // Test Jenkins
        temp_env::with_var_unset("CI", || {
            temp_env::with_var("JENKINS_URL", Some("http://jenkins.example.com"), || {
                assert!(is_ci());
            });
        });
    }

    #[test]
    fn test_is_ci_not_detected() {
        // Clear all CI-related environment variables
        temp_env::with_vars_unset(
            vec![
                "CI",
                "GITHUB_ACTIONS",
                "GITLAB_CI",
                "BUILDKITE",
                "JENKINS_URL",
                "CIRCLECI",
                "TRAVIS",
                "BITBUCKET_PIPELINES",
                "AZURE_PIPELINES",
                "TF_BUILD",
                "DRONE",
                "TEAMCITY_VERSION",
            ],
            || {
                assert!(!is_ci());
            },
        );
    }

    #[test]
    fn test_approval_status_auto_approved_in_ci() {
        let manager = ApprovalManager::new(PathBuf::from("/tmp/test"));
        let directory = Path::new("/test/ci_dir");

        // Create hooks that would normally require approval
        let mut hooks_map = HashMap::new();
        hooks_map.insert("setup".to_string(), make_hook("echo", &["hello"]));

        let hooks = Hooks {
            on_enter: Some(hooks_map),
            on_exit: None,
            pre_push: None,
        };

        // In CI environment, should be auto-approved
        temp_env::with_var("CI", Some("true"), || {
            let status = check_approval_status(&manager, directory, Some(&hooks)).unwrap();
            assert!(
                matches!(status, ApprovalStatus::Approved),
                "Hooks should be auto-approved in CI"
            );
        });

        // Outside CI environment, should require approval
        temp_env::with_vars_unset(
            vec![
                "CI",
                "GITHUB_ACTIONS",
                "GITLAB_CI",
                "BUILDKITE",
                "JENKINS_URL",
                "CIRCLECI",
                "TRAVIS",
                "BITBUCKET_PIPELINES",
                "AZURE_PIPELINES",
                "TF_BUILD",
                "DRONE",
                "TEAMCITY_VERSION",
            ],
            || {
                let status = check_approval_status(&manager, directory, Some(&hooks)).unwrap();
                assert!(
                    matches!(status, ApprovalStatus::NotApproved { .. }),
                    "Hooks should require approval outside CI"
                );
            },
        );
    }
}
