//! CODEOWNERS sync providers for different platforms.
//!
//! This module provides a trait-based abstraction for syncing CODEOWNERS files
//! across different platforms (GitHub, GitLab, Bitbucket). Each platform has
//! specific requirements for file location and section syntax.
//!
//! # Provider Implementations
//!
//! Provider implementations are available in separate platform crates:
//! - `cuenv-github`: [`GitHubCodeOwnersProvider`](https://docs.rs/cuenv-github)
//! - `cuenv-gitlab`: [`GitLabCodeOwnersProvider`](https://docs.rs/cuenv-gitlab)
//! - `cuenv-bitbucket`: [`BitbucketCodeOwnersProvider`](https://docs.rs/cuenv-bitbucket)
//!
//! # Example
//!
//! ```rust,ignore
//! use cuenv_codeowners::provider::{CodeOwnersProvider, ProjectOwners};
//! use cuenv_github::GitHubCodeOwnersProvider;
//! use std::path::Path;
//!
//! let provider = GitHubCodeOwnersProvider;
//! let projects = vec![/* ... */];
//! let result = provider.sync(Path::new("."), &projects, false)?;
//! ```

use crate::{CodeOwnersBuilder, Platform, Rule};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Error type for provider operations.
#[derive(Debug)]
pub enum ProviderError {
    /// I/O error during file operations.
    Io(io::Error),
    /// Path validation error (e.g., path traversal attempt).
    InvalidPath(String),
    /// Configuration error.
    Configuration(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::InvalidPath(msg) => write!(f, "Invalid path: {msg}"),
            Self::Configuration(msg) => write!(f, "Configuration error: {msg}"),
        }
    }
}

impl std::error::Error for ProviderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for ProviderError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Result type for provider operations.
pub type Result<T> = std::result::Result<T, ProviderError>;

/// Project with its owners configuration and relative path.
///
/// Used to aggregate ownership rules from multiple projects in a workspace.
#[derive(Debug, Clone)]
pub struct ProjectOwners {
    /// Relative path from repo root to project directory.
    pub path: PathBuf,
    /// Project name (used for section headers).
    pub name: String,
    /// Ownership rules for this project.
    pub rules: Vec<Rule>,
}

impl ProjectOwners {
    /// Create a new project owners configuration.
    pub fn new(path: impl Into<PathBuf>, name: impl Into<String>, rules: Vec<Rule>) -> Self {
        Self {
            path: path.into(),
            name: name.into(),
            rules,
        }
    }
}

/// Status of a sync operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    /// File was created (didn't exist before).
    Created,
    /// File was updated (content changed).
    Updated,
    /// File is unchanged (content matches).
    Unchanged,
    /// Would create file (dry-run mode).
    WouldCreate,
    /// Would update file (dry-run mode).
    WouldUpdate,
}

impl std::fmt::Display for SyncStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Created => write!(f, "created"),
            Self::Updated => write!(f, "updated"),
            Self::Unchanged => write!(f, "unchanged"),
            Self::WouldCreate => write!(f, "would create"),
            Self::WouldUpdate => write!(f, "would update"),
        }
    }
}

/// Result of a sync operation.
#[derive(Debug, Clone)]
pub struct SyncResult {
    /// Path where the file was written (or would be written).
    pub path: PathBuf,
    /// Status of the operation.
    pub status: SyncStatus,
    /// Generated content.
    pub content: String,
}

/// Result of a check operation.
#[derive(Debug, Clone)]
pub struct CheckResult {
    /// Path to the CODEOWNERS file.
    pub path: PathBuf,
    /// Whether the file is in sync with configuration.
    pub in_sync: bool,
    /// Expected content (from configuration).
    pub expected: String,
    /// Actual content (from file), if file exists.
    pub actual: Option<String>,
}

/// Trait for CODEOWNERS sync providers.
///
/// Each platform (GitHub, GitLab, Bitbucket) implements this trait to provide
/// platform-specific sync behavior.
pub trait CodeOwnersProvider: Send + Sync {
    /// Get the platform type.
    fn platform(&self) -> Platform;

    /// Sync CODEOWNERS from project configurations.
    ///
    /// Aggregates ownership rules from all projects and writes the appropriate
    /// CODEOWNERS file(s) for this platform.
    ///
    /// # Arguments
    ///
    /// * `repo_root` - Root directory of the repository
    /// * `projects` - List of projects with their ownership configurations
    /// * `dry_run` - If true, don't write files, just report what would happen
    ///
    /// # Errors
    ///
    /// Returns an error if file operations fail or configuration is invalid.
    fn sync(
        &self,
        repo_root: &Path,
        projects: &[ProjectOwners],
        dry_run: bool,
    ) -> Result<SyncResult>;

    /// Check if CODEOWNERS is in sync with configuration.
    ///
    /// # Arguments
    ///
    /// * `repo_root` - Root directory of the repository
    /// * `projects` - List of projects with their ownership configurations
    ///
    /// # Errors
    ///
    /// Returns an error if file operations fail or configuration is invalid.
    fn check(&self, repo_root: &Path, projects: &[ProjectOwners]) -> Result<CheckResult>;
}

/// Prefix a pattern with the project's relative path.
///
/// This ensures patterns in nested projects correctly reference files
/// from the repository root in the aggregated CODEOWNERS file.
///
/// # Examples
///
/// ```rust,ignore
/// // Root project - patterns are normalized to start with /
/// prefix_pattern("", "*.rs") -> "/*.rs"
/// prefix_pattern(".", "/docs/**") -> "/docs/**"
///
/// // Nested project - patterns are prefixed with project path
/// prefix_pattern("services/api", "*.rs") -> "/services/api/*.rs"
/// prefix_pattern("services/api", "/src/**") -> "/services/api/src/**"
/// ```
pub fn prefix_pattern(project_path: &Path, pattern: &str) -> String {
    let prefix = project_path.to_string_lossy();

    // Root project (empty or "." path) - normalize to start with /
    if prefix.is_empty() || prefix == "." {
        if pattern.starts_with('/') {
            pattern.to_string()
        } else {
            format!("/{pattern}")
        }
    }
    // Nested project - prefix with project path
    else if pattern.starts_with('/') {
        // Pattern like "/src/**" becomes "/project/path/src/**"
        format!("/{prefix}{pattern}")
    } else {
        // Pattern like "*.rs" becomes "/project/path/*.rs"
        format!("/{prefix}/{pattern}")
    }
}

/// Generate aggregated CODEOWNERS content from multiple projects.
///
/// This is the core aggregation logic used by all providers. Each provider
/// can customize the output format (e.g., section syntax) by setting the
/// platform on the builder.
pub fn generate_aggregated_content(
    platform: Platform,
    projects: &[ProjectOwners],
    header: Option<&str>,
) -> String {
    let mut builder = CodeOwnersBuilder::default().platform(platform);

    // Set header
    let default_header = "CODEOWNERS file - Generated by cuenv\n\
                          Do not edit manually. Run `cuenv sync codeowners -A` to regenerate.";
    builder = builder.header(header.unwrap_or(default_header));

    // Process each project
    for project in projects {
        // Add rules with prefixed patterns
        for rule in &project.rules {
            let prefixed_pattern = prefix_pattern(&project.path, &rule.pattern);
            let mut new_rule = Rule::new(prefixed_pattern, rule.owners.clone());

            // Use project name as section if rule doesn't have one
            if let Some(ref section) = rule.section {
                new_rule = new_rule.section(section.clone());
            } else {
                new_rule = new_rule.section(project.name.clone());
            }

            if let Some(ref description) = rule.description {
                new_rule = new_rule.description(description.clone());
            }

            builder = builder.rule(new_rule);
        }
    }

    builder.build().generate()
}

/// Write content to a file, creating parent directories as needed.
///
/// Returns the sync status based on whether the file was created, updated, or unchanged.
pub fn write_codeowners_file(path: &Path, content: &str, dry_run: bool) -> Result<SyncStatus> {
    let exists = path.exists();
    let current_content = if exists {
        Some(fs::read_to_string(path)?)
    } else {
        None
    };

    // Check if content matches (normalize line endings for comparison)
    let normalize = |s: &str| -> String {
        s.replace("\r\n", "\n")
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    };

    let content_matches = current_content
        .as_ref()
        .is_some_and(|current| normalize(current) == normalize(content));

    if content_matches {
        return Ok(SyncStatus::Unchanged);
    }

    if dry_run {
        return Ok(if exists {
            SyncStatus::WouldUpdate
        } else {
            SyncStatus::WouldCreate
        });
    }

    // Create parent directories if needed
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, content)?;

    Ok(if exists {
        SyncStatus::Updated
    } else {
        SyncStatus::Created
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prefix_pattern_root_project() {
        // Root project patterns should start with /
        assert_eq!(prefix_pattern(Path::new(""), "*.rs"), "/*.rs");
        assert_eq!(prefix_pattern(Path::new("."), "*.rs"), "/*.rs");
        assert_eq!(prefix_pattern(Path::new(""), "/docs/**"), "/docs/**");
        assert_eq!(prefix_pattern(Path::new("."), "/src/**"), "/src/**");
    }

    #[test]
    fn test_prefix_pattern_nested_project() {
        // Nested project patterns should be prefixed
        assert_eq!(
            prefix_pattern(Path::new("services/api"), "*.rs"),
            "/services/api/*.rs"
        );
        assert_eq!(
            prefix_pattern(Path::new("services/api"), "/src/**"),
            "/services/api/src/**"
        );
        assert_eq!(
            prefix_pattern(Path::new("libs/common"), "Cargo.toml"),
            "/libs/common/Cargo.toml"
        );
    }

    #[test]
    fn test_generate_aggregated_content() {
        let projects = vec![
            ProjectOwners::new(
                "services/api",
                "services/api",
                vec![Rule::new("*.rs", ["@backend-team"])],
            ),
            ProjectOwners::new(
                "services/web",
                "services/web",
                vec![Rule::new("*.ts", ["@frontend-team"])],
            ),
        ];

        let content = generate_aggregated_content(Platform::Github, &projects, None);

        assert!(content.contains("/services/api/*.rs @backend-team"));
        assert!(content.contains("/services/web/*.ts @frontend-team"));
        assert!(content.contains("# services/api"));
        assert!(content.contains("# services/web"));
    }
}
