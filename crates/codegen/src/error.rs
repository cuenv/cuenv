//! Code generation errors.

use thiserror::Error;

/// Errors that can occur during code generation
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum CodegenError {
    /// Error loading or evaluating CUE codegen configuration
    #[error("Codegen error: {0}")]
    Codegen(String),

    /// Error during file generation
    #[error("Generation error: {0}")]
    Generation(String),

    /// Error during formatting
    #[error("Formatting error: {0}")]
    Formatting(String),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON error
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Result type for codegen operations
pub type Result<T> = std::result::Result<T, CodegenError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_codegen_error_codegen_display() {
        let error = CodegenError::Codegen("test codegen error".to_string());
        assert_eq!(error.to_string(), "Codegen error: test codegen error");
    }

    #[test]
    fn test_codegen_error_generation_display() {
        let error = CodegenError::Generation("test generation error".to_string());
        assert_eq!(error.to_string(), "Generation error: test generation error");
    }

    #[test]
    fn test_codegen_error_formatting_display() {
        let error = CodegenError::Formatting("test formatting error".to_string());
        assert_eq!(error.to_string(), "Formatting error: test formatting error");
    }

    #[test]
    fn test_codegen_error_io_from() {
        let io_error = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let error: CodegenError = io_error.into();
        assert!(matches!(error, CodegenError::Io(_)));
        assert!(error.to_string().contains("file not found"));
    }

    #[test]
    fn test_codegen_error_json_from() {
        let json_str = "{ invalid json }";
        let json_error = serde_json::from_str::<serde_json::Value>(json_str).unwrap_err();
        let error: CodegenError = json_error.into();
        assert!(matches!(error, CodegenError::Json(_)));
        assert!(error.to_string().contains("JSON error"));
    }
}
