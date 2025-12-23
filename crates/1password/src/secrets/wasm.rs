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
}
