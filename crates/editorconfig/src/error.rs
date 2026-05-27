// ============================================================================
// Error types
// ============================================================================

/// Errors that can occur during EditorConfig file generation.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid EditorConfig section pattern.
    #[error("Invalid EditorConfig section pattern '{pattern}': {reason}")]
    InvalidSectionPattern {
        /// The invalid section pattern.
        pattern: String,
        /// Reason why the pattern is invalid.
        reason: String,
    },

    /// IO error during file operations.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result type for EditorConfig operations.
pub type Result<T> = std::result::Result<T, Error>;
