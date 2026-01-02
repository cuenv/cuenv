//! Input validation functions

use crate::error::{CueEngineError as Error, Result};
use std::path::Path;

/// Configuration limits for CUE evaluation
#[derive(Debug, Clone)]
pub struct Limits {
    /// Maximum path length in characters
    pub max_path_length: usize,
    /// Maximum package name length in characters
    pub max_package_name_length: usize,
    /// Maximum output size in bytes
    pub max_output_size: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_path_length: 4096,
            max_package_name_length: 256,
            max_output_size: 100 * 1024 * 1024, // 100MB
        }
    }
}

/// Validate a directory path
///
/// # Errors
///
/// Returns an error if the path doesn't exist, isn't a directory, is too long,
/// or contains parent directory traversal
pub fn validate_path(path: &Path, limits: &Limits) -> Result<()> {
    // Check path exists
    if !path.exists() {
        return Err(Error::validation(format!(
            "Path does not exist: {}",
            path.display()
        )));
    }

    // Check path is a directory
    if !path.is_dir() {
        return Err(Error::validation(format!(
            "Path is not a directory: {}",
            path.display()
        )));
    }

    // Check path length
    if let Some(path_str) = path.to_str()
        && path_str.len() > limits.max_path_length
    {
        return Err(Error::validation(format!(
            "Path exceeds maximum length of {} characters",
            limits.max_path_length
        )));
    }

    // Check for path traversal attempts
    for component in path.components() {
        if component == std::path::Component::ParentDir {
            return Err(Error::validation(
                "Path contains parent directory traversal (..)",
            ));
        }
    }

    Ok(())
}

/// Validate a package name
///
/// # Errors
///
/// Returns an error if the name is empty, too long, contains invalid characters,
/// or doesn't start with an alphabetic character or underscore
pub fn validate_package_name(name: &str, limits: &Limits) -> Result<()> {
    // Check length
    if name.is_empty() {
        return Err(Error::validation("Package name cannot be empty"));
    }

    if name.len() > limits.max_package_name_length {
        return Err(Error::validation(format!(
            "Package name exceeds maximum length of {} characters",
            limits.max_package_name_length
        )));
    }

    // Check for valid package name characters
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Err(Error::validation(
            "Package name contains invalid characters (only alphanumeric, underscore, and hyphen allowed)",
        ));
    }

    // Check first character is alphabetic or underscore (CUE allows underscore-prefixed "hidden" packages)
    // SAFETY: We already verified `name.is_empty()` returns false above
    let Some(first_char) = name.chars().next() else {
        unreachable!("name is non-empty (checked above)")
    };
    if !first_char.is_alphabetic() && first_char != '_' {
        return Err(Error::validation(
            "Package name must start with an alphabetic character or underscore",
        ));
    }

    Ok(())
}

/// Validate output size
///
/// # Errors
///
/// Returns an error if the output exceeds the maximum size limit
pub fn validate_output(output: &str, limits: &Limits) -> Result<()> {
    if output.len() > limits.max_output_size {
        return Err(Error::validation(format!(
            "Output exceeds maximum size of {} bytes",
            limits.max_output_size
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_limits_default() {
        let limits = Limits::default();
        assert_eq!(limits.max_path_length, 4096);
        assert_eq!(limits.max_package_name_length, 256);
        assert_eq!(limits.max_output_size, 100 * 1024 * 1024);
    }

    #[test]
    fn test_limits_clone() {
        let limits = Limits::default();
        let cloned = limits.clone();
        assert_eq!(limits.max_path_length, cloned.max_path_length);
    }

    #[test]
    fn test_validate_path_nonexistent() {
        let limits = Limits::default();
        let result = validate_path(Path::new("/nonexistent/path"), &limits);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn test_validate_path_not_directory() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        std::fs::write(&file_path, "test").unwrap();

        let limits = Limits::default();
        let result = validate_path(&file_path, &limits);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not a directory"));
    }

    #[test]
    fn test_validate_path_success() {
        let temp_dir = TempDir::new().unwrap();
        let limits = Limits::default();
        let result = validate_path(temp_dir.path(), &limits);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_path_too_long() {
        let temp_dir = TempDir::new().unwrap();
        let limits = Limits {
            max_path_length: 5,
            ..Default::default()
        };
        let result = validate_path(temp_dir.path(), &limits);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("exceeds maximum length"));
    }

    #[test]
    fn test_validate_package_name_empty() {
        let limits = Limits::default();
        let result = validate_package_name("", &limits);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cannot be empty"));
    }

    #[test]
    fn test_validate_package_name_too_long() {
        let limits = Limits {
            max_package_name_length: 5,
            ..Default::default()
        };
        let result = validate_package_name("toolong", &limits);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("exceeds maximum length"));
    }

    #[test]
    fn test_validate_package_name_invalid_chars() {
        let limits = Limits::default();
        let result = validate_package_name("invalid@name", &limits);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid characters"));
    }

    #[test]
    fn test_validate_package_name_invalid_start() {
        let limits = Limits::default();
        let result = validate_package_name("123abc", &limits);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must start with"));
    }

    #[test]
    fn test_validate_package_name_success() {
        let limits = Limits::default();
        assert!(validate_package_name("valid_name", &limits).is_ok());
        assert!(validate_package_name("_hidden", &limits).is_ok());
        assert!(validate_package_name("with-hyphen", &limits).is_ok());
        assert!(validate_package_name("MixedCase123", &limits).is_ok());
    }

    #[test]
    fn test_validate_output_success() {
        let limits = Limits::default();
        let result = validate_output("small output", &limits);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_output_too_large() {
        let limits = Limits {
            max_output_size: 10,
            ..Default::default()
        };
        let result = validate_output("this output is too large", &limits);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("exceeds maximum size"));
    }
}
