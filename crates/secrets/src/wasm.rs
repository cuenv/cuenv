//! WASM loading utilities for 1Password SDK
//!
//! This module provides utilities to load the 1Password WASM SDK from the cache.
//! The WASM must be downloaded first using `cuenv secrets setup onepassword`.

use crate::SecretError;
use std::path::PathBuf;

/// Get the path to the 1Password WASM SDK in the cache
#[cfg(feature = "onepassword")]
pub fn onepassword_wasm_path() -> Result<PathBuf, SecretError> {
    let cache_dir = dirs::cache_dir().ok_or_else(|| SecretError::ResolutionFailed {
        name: "onepassword".to_string(),
        message: "Could not determine cache directory".to_string(),
    })?;

    Ok(cache_dir.join("cuenv").join("wasm").join("onepassword-core.wasm"))
}

/// Check if the 1Password WASM SDK is available in the cache
#[cfg(feature = "onepassword")]
pub fn onepassword_wasm_available() -> bool {
    onepassword_wasm_path()
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// Load the 1Password WASM SDK from the cache
#[cfg(feature = "onepassword")]
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
    #[cfg(feature = "onepassword")]
    fn test_wasm_path() {
        let path = onepassword_wasm_path().unwrap();
        assert!(path.to_string_lossy().contains("onepassword-core.wasm"));
    }
}
