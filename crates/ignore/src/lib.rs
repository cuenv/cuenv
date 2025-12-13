//! Ignore file generation for cuenv.
//!
//! This crate provides functionality to generate tool-specific ignore files
//! (.gitignore, .dockerignore, etc.) from a declarative configuration.
//!
//! # Example
//!
//! ```no_run
//! use cuenv_ignore::{generate_ignore_files, IgnoreConfig};
//! use std::path::Path;
//!
//! let configs = vec![
//!     IgnoreConfig {
//!         tool: "git".to_string(),
//!         patterns: vec!["node_modules/".to_string(), ".env".to_string()],
//!         filename: None,
//!     },
//! ];
//!
//! let result = generate_ignore_files(Path::new("."), configs, false);
//! ```

use std::path::Path;

/// Configuration for generating a single ignore file.
#[derive(Debug, Clone)]
pub struct IgnoreConfig {
    /// Tool name (e.g., "git", "docker", "npm").
    /// Used to generate the filename as `.{tool}ignore` unless overridden.
    pub tool: String,
    /// List of patterns to include in the ignore file.
    pub patterns: Vec<String>,
    /// Optional filename override. If None, defaults to `.{tool}ignore`.
    pub filename: Option<String>,
}

/// Result of generating ignore files.
#[derive(Debug)]
pub struct SyncResult {
    /// Results for each file that was processed.
    pub files: Vec<FileResult>,
}

/// Result for a single ignore file.
#[derive(Debug)]
pub struct FileResult {
    /// The filename that was generated (e.g., ".gitignore").
    pub filename: String,
    /// The status of the file operation.
    pub status: FileStatus,
    /// Number of patterns in the file.
    pub pattern_count: usize,
}

/// Status of a file operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    /// File was newly created.
    Created,
    /// File existed and was updated with new content.
    Updated,
    /// File existed and content was unchanged.
    Unchanged,
    /// Would be created (dry-run mode).
    WouldCreate,
    /// Would be updated (dry-run mode).
    WouldUpdate,
}

impl std::fmt::Display for FileStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Created => write!(f, "Created"),
            Self::Updated => write!(f, "Updated"),
            Self::Unchanged => write!(f, "Unchanged"),
            Self::WouldCreate => write!(f, "Would create"),
            Self::WouldUpdate => write!(f, "Would update"),
        }
    }
}

/// Errors that can occur during ignore file generation.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid tool name (contains path separators or is empty).
    #[error("Invalid tool name '{name}': {reason}")]
    InvalidToolName {
        /// The invalid tool name.
        name: String,
        /// Reason why it's invalid.
        reason: String,
    },

    /// Not inside a Git repository.
    #[error("cuenv sync ignore must be run within a Git repository")]
    NotInGitRepo,

    /// Cannot operate in a bare repository.
    #[error("Cannot sync in a bare Git repository")]
    BareRepository,

    /// Target directory is outside the Git repository.
    #[error("Target directory must be within the Git repository")]
    OutsideGitRepo,

    /// IO error during file operations.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result type for ignore operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Generate ignore files from the given configurations.
///
/// # Arguments
///
/// * `dir` - Directory where ignore files will be generated
/// * `configs` - List of ignore configurations
/// * `dry_run` - If true, don't write files, just report what would happen
///
/// # Errors
///
/// Returns an error if:
/// - The directory is not within a Git repository
/// - A tool name contains invalid characters (path separators)
/// - File I/O fails
///
/// # Example
///
/// ```no_run
/// use cuenv_ignore::{generate_ignore_files, IgnoreConfig};
/// use std::path::Path;
///
/// let configs = vec![
///     IgnoreConfig {
///         tool: "git".to_string(),
///         patterns: vec!["node_modules/".to_string()],
///         filename: None,
///     },
/// ];
///
/// let result = generate_ignore_files(Path::new("."), configs, false)?;
/// for file in &result.files {
///     println!("{}: {} ({} patterns)", file.status, file.filename, file.pattern_count);
/// }
/// # Ok::<(), cuenv_ignore::Error>(())
/// ```
pub fn generate_ignore_files(
    dir: &Path,
    configs: Vec<IgnoreConfig>,
    dry_run: bool,
) -> Result<SyncResult> {
    tracing::info!("Starting ignore file generation");

    // Verify we're in a git repository
    verify_git_repository(dir)?;

    let mut results = Vec::new();

    // Sort configs by tool name for deterministic output
    let mut sorted_configs = configs;
    sorted_configs.sort_by(|a, b| a.tool.cmp(&b.tool));

    for config in sorted_configs {
        // Skip empty pattern lists
        if config.patterns.is_empty() {
            tracing::debug!("Skipping tool '{}' - no patterns", config.tool);
            continue;
        }

        // Validate tool name
        validate_tool_name(&config.tool)?;

        // Get filename (use override or default)
        let filename = get_ignore_filename(&config.tool, config.filename.as_deref());

        // Validate filename doesn't contain path separators
        validate_filename(&filename)?;

        let filepath = dir.join(&filename);
        let content = generate_ignore_content(&config.patterns);

        let (status, pattern_count) = if dry_run {
            let status = if filepath.exists() {
                let existing = std::fs::read_to_string(&filepath)?;
                if existing == content {
                    FileStatus::Unchanged
                } else {
                    FileStatus::WouldUpdate
                }
            } else {
                FileStatus::WouldCreate
            };
            (status, config.patterns.len())
        } else {
            let status = write_ignore_file(&filepath, &content)?;
            (status, config.patterns.len())
        };

        tracing::info!(
            filename = %filename,
            status = %status,
            patterns = pattern_count,
            "Processed ignore file"
        );

        results.push(FileResult {
            filename,
            status,
            pattern_count,
        });
    }

    Ok(SyncResult { files: results })
}

/// Verify that the directory is within a Git repository.
fn verify_git_repository(dir: &Path) -> Result<()> {
    let repo = gix::discover(dir).map_err(|e| {
        tracing::debug!("Git discovery failed: {}", e);
        Error::NotInGitRepo
    })?;

    let git_root = repo.workdir().ok_or(Error::BareRepository)?;

    // Canonicalize paths for comparison
    let canonical_dir = std::fs::canonicalize(dir)?;
    let canonical_git = std::fs::canonicalize(git_root)?;

    if !canonical_dir.starts_with(&canonical_git) {
        return Err(Error::OutsideGitRepo);
    }

    tracing::debug!(
        git_root = %canonical_git.display(),
        target_dir = %canonical_dir.display(),
        "Verified directory is within Git repository"
    );

    Ok(())
}

/// Validate that a tool name doesn't contain path separators.
fn validate_tool_name(tool: &str) -> Result<()> {
    if tool.is_empty() {
        return Err(Error::InvalidToolName {
            name: tool.to_string(),
            reason: "tool name cannot be empty".to_string(),
        });
    }

    if tool.contains('/') || tool.contains('\\') {
        return Err(Error::InvalidToolName {
            name: tool.to_string(),
            reason: "tool name cannot contain path separators".to_string(),
        });
    }

    if tool.contains("..") {
        return Err(Error::InvalidToolName {
            name: tool.to_string(),
            reason: "tool name cannot contain parent directory references".to_string(),
        });
    }

    Ok(())
}

/// Validate that a filename doesn't contain path separators.
fn validate_filename(filename: &str) -> Result<()> {
    if filename.contains('/') || filename.contains('\\') {
        return Err(Error::InvalidToolName {
            name: filename.to_string(),
            reason: "filename cannot contain path separators".to_string(),
        });
    }

    if filename.contains("..") {
        return Err(Error::InvalidToolName {
            name: filename.to_string(),
            reason: "filename cannot contain parent directory references".to_string(),
        });
    }

    Ok(())
}

/// Get the ignore filename for a tool.
fn get_ignore_filename(tool: &str, override_filename: Option<&str>) -> String {
    override_filename.map_or_else(|| format!(".{tool}ignore"), String::from)
}

/// Generate the content for an ignore file.
fn generate_ignore_content(patterns: &[String]) -> String {
    let mut lines = vec![
        "# Generated by cuenv - do not edit".to_string(),
        "# Source: env.cue".to_string(),
        String::new(),
    ];
    lines.extend(patterns.iter().cloned());
    format!("{}\n", lines.join("\n"))
}

/// Write an ignore file and return the status.
fn write_ignore_file(filepath: &Path, content: &str) -> Result<FileStatus> {
    let status = if filepath.exists() {
        let existing = std::fs::read_to_string(filepath)?;
        if existing == content {
            return Ok(FileStatus::Unchanged);
        }
        FileStatus::Updated
    } else {
        FileStatus::Created
    };

    std::fs::write(filepath, content)?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_ignore_filename_default() {
        assert_eq!(get_ignore_filename("git", None), ".gitignore");
        assert_eq!(get_ignore_filename("docker", None), ".dockerignore");
        assert_eq!(get_ignore_filename("npm", None), ".npmignore");
        assert_eq!(get_ignore_filename("custom", None), ".customignore");
    }

    #[test]
    fn test_get_ignore_filename_override() {
        assert_eq!(
            get_ignore_filename("git", Some(".my-gitignore")),
            ".my-gitignore"
        );
        assert_eq!(
            get_ignore_filename("custom", Some(".special")),
            ".special"
        );
    }

    #[test]
    fn test_validate_tool_name_valid() {
        assert!(validate_tool_name("git").is_ok());
        assert!(validate_tool_name("docker").is_ok());
        assert!(validate_tool_name("my-custom-tool").is_ok());
        assert!(validate_tool_name("tool_with_underscore").is_ok());
    }

    #[test]
    fn test_validate_tool_name_invalid() {
        // Empty
        assert!(validate_tool_name("").is_err());

        // Path separators
        assert!(validate_tool_name("../etc").is_err());
        assert!(validate_tool_name("foo/bar").is_err());
        assert!(validate_tool_name("foo\\bar").is_err());

        // Parent directory reference
        assert!(validate_tool_name("..").is_err());
        assert!(validate_tool_name("foo..bar").is_err());
    }

    #[test]
    fn test_generate_ignore_content() {
        let patterns = vec![
            "node_modules/".to_string(),
            ".env".to_string(),
            "*.log".to_string(),
        ];
        let content = generate_ignore_content(&patterns);

        assert!(content.starts_with("# Generated by cuenv - do not edit"));
        assert!(content.contains("# Source: env.cue"));
        assert!(content.contains("node_modules/"));
        assert!(content.contains(".env"));
        assert!(content.contains("*.log"));
        assert!(content.ends_with('\n'));
    }

    #[test]
    fn test_generate_ignore_content_empty() {
        let patterns: Vec<String> = vec![];
        let content = generate_ignore_content(&patterns);

        assert!(content.starts_with("# Generated by cuenv - do not edit"));
        assert!(content.contains("# Source: env.cue"));
        assert!(content.ends_with('\n'));
    }

    #[test]
    fn test_file_status_display() {
        assert_eq!(FileStatus::Created.to_string(), "Created");
        assert_eq!(FileStatus::Updated.to_string(), "Updated");
        assert_eq!(FileStatus::Unchanged.to_string(), "Unchanged");
        assert_eq!(FileStatus::WouldCreate.to_string(), "Would create");
        assert_eq!(FileStatus::WouldUpdate.to_string(), "Would update");
    }
}
