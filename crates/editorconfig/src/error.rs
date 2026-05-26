// ============================================================================
// Error types
// ============================================================================

/// Errors that can occur during EditorConfig file generation.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// IO error during file operations.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result type for EditorConfig operations.
pub type Result<T> = std::result::Result<T, Error>;
