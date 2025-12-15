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
        if let std::path::Component::ParentDir = component {
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
    let first_char = name
        .chars()
        .next()
        .expect("name is non-empty (checked above)");
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
