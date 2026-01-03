//! WASM loading utilities for 1Password SDK
//!
//! This module provides utilities to load the 1Password WASM SDK from the cache.
//! The WASM must be downloaded first using `cuenv secrets setup onepassword`.

use cuenv_secrets::SecretError;
use std::path::PathBuf;

/// Get the path to the 1Password WASM SDK
///
/// Checks `ONEPASSWORD_WASM_PATH` environment variable first (used in Nix builds),
/// then falls back to the cache directory for local development.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be determined and no env var is set.
pub fn onepassword_wasm_path() -> Result<PathBuf, SecretError> {
    // Check environment override first (used in Nix builds)
    if let Ok(path) = std::env::var("ONEPASSWORD_WASM_PATH") {
        return Ok(PathBuf::from(path));
    }

    // Fall back to cache directory for local development
    let cache_dir = dirs::cache_dir().ok_or_else(|| SecretError::ResolutionFailed {
        name: "onepassword".to_string(),
        message: "Could not determine cache directory".to_string(),
    })?;

    Ok(cache_dir
        .join("cuenv")
        .join("wasm")
        .join("onepassword-core.wasm"))
}

/// Check if the 1Password WASM SDK is available in the cache
#[must_use]
pub fn onepassword_wasm_available() -> bool {
    onepassword_wasm_path().map(|p| p.exists()).unwrap_or(false)
}

/// Load the 1Password WASM SDK from the cache
///
/// # Errors
///
/// Returns an error if:
/// - The cache directory cannot be determined
/// - The WASM file does not exist (run `cuenv secrets setup onepassword` first)
/// - The file cannot be read
pub fn load_onepassword_wasm() -> Result<Vec<u8>, SecretError> {
    let path = onepassword_wasm_path()?;

    if !path.exists() {
        return Err(SecretError::ResolutionFailed {
            name: "onepassword".to_string(),
            message: format!(
                "1Password WASM SDK not found. Run 'cuenv secrets setup onepassword' to download it.\n\
                Expected at: {}",
                path.display()
            ),
        });
    }

    std::fs::read(&path).map_err(|e| SecretError::ResolutionFailed {
        name: "onepassword".to_string(),
        message: format!("Failed to read WASM file: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wasm_path() {
        let path = onepassword_wasm_path().unwrap();

        // If ONEPASSWORD_WASM_PATH env var is set (Nix builds), path should match it.
        // Otherwise, path should be in cache directory with standard filename.
        if let Ok(env_path) = std::env::var("ONEPASSWORD_WASM_PATH") {
            assert_eq!(path, PathBuf::from(env_path));
        } else {
            assert!(path.to_string_lossy().contains("onepassword-core.wasm"));
        }
    }

    #[test]
    fn test_wasm_path_contains_cuenv_directory() {
        // Unless overridden by env, path should be in cuenv cache directory
        if std::env::var("ONEPASSWORD_WASM_PATH").is_err() {
            let path = onepassword_wasm_path().unwrap();
            let path_str = path.to_string_lossy();
            assert!(path_str.contains("cuenv"));
            assert!(path_str.contains("wasm"));
        }
    }

    #[test]
    fn test_wasm_path_is_absolute() {
        let path = onepassword_wasm_path().unwrap();
        assert!(path.is_absolute(), "WASM path should be absolute");
    }

    #[test]
    fn test_wasm_available_returns_boolean() {
        // Just verify the function returns without panic
        let result = onepassword_wasm_available();
        // Result should be boolean
        let _ = result;
    }

    #[test]
    fn test_wasm_available_consistency() {
        // Multiple calls should return the same result
        let first = onepassword_wasm_available();
        let second = onepassword_wasm_available();
        assert_eq!(first, second);
    }

    #[test]
    fn test_load_wasm_missing_file() {
        // When WASM file doesn't exist, should return appropriate error
        // Only test if env var is not set (otherwise it might point to real file)
        if std::env::var("ONEPASSWORD_WASM_PATH").is_err() {
            let result = load_onepassword_wasm();
            // If the file is actually installed, it will succeed
            // Otherwise it should fail with a helpful message
            if let Err(err) = result {
                let err_msg = format!("{err:?}");
                assert!(
                    err_msg.contains("WASM SDK not found") || err_msg.contains("not found"),
                    "Error should mention WASM not found: {err_msg}"
                );
            }
        }
    }

    #[test]
    fn test_wasm_path_file_extension() {
        let path = onepassword_wasm_path().unwrap();
        let extension = path.extension().and_then(|e| e.to_str());
        // If path is from env, it might have any extension
        // But typically it should be .wasm
        if std::env::var("ONEPASSWORD_WASM_PATH").is_err() {
            assert_eq!(extension, Some("wasm"), "Path should have .wasm extension");
        }
    }

    #[test]
    fn test_wasm_path_has_filename() {
        let path = onepassword_wasm_path().unwrap();
        assert!(path.file_name().is_some(), "Path should have a filename");
    }

    #[test]
    fn test_wasm_path_parent_exists_or_is_subpath() {
        let path = onepassword_wasm_path().unwrap();
        // Parent should be determinable
        assert!(path.parent().is_some(), "Path should have a parent");
    }

    #[test]
    fn test_wasm_available_matches_path_exists() {
        let path_result = onepassword_wasm_path();
        let available = onepassword_wasm_available();

        // If path exists, available should be true
        if let Ok(path) = path_result
            && path.exists()
        {
            assert!(available, "Should be available when path exists");
        }
    }

    #[test]
    fn test_wasm_path_ends_with_expected_filename() {
        // Unless overridden, the filename should be standard
        if std::env::var("ONEPASSWORD_WASM_PATH").is_err() {
            let path = onepassword_wasm_path().unwrap();
            let filename = path.file_name().unwrap().to_string_lossy();
            assert_eq!(filename, "onepassword-core.wasm");
        }
    }

    #[test]
    fn test_load_wasm_error_message_contains_path() {
        // Only test if file doesn't exist
        if std::env::var("ONEPASSWORD_WASM_PATH").is_err() && !onepassword_wasm_available() {
            let result = load_onepassword_wasm();
            if let Err(err) = result {
                let err_msg = format!("{err:?}");
                // Error should include path for debugging
                assert!(
                    err_msg.contains("onepassword-core.wasm") || err_msg.contains("Expected at"),
                    "Error should include path info: {err_msg}"
                );
            }
        }
    }

    #[test]
    fn test_load_wasm_error_suggests_setup_command() {
        // Only test if file doesn't exist
        if std::env::var("ONEPASSWORD_WASM_PATH").is_err() && !onepassword_wasm_available() {
            let result = load_onepassword_wasm();
            if let Err(err) = result {
                let err_msg = format!("{err:?}");
                // Error should suggest setup command
                assert!(
                    err_msg.contains("cuenv secrets setup onepassword"),
                    "Error should suggest setup command: {err_msg}"
                );
            }
        }
    }
}
