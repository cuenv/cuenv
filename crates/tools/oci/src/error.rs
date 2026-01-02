//! Error types for OCI provider operations.

use thiserror::Error;

/// Result type for OCI provider operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur during OCI operations.
#[derive(Error, Debug)]
pub enum Error {
    /// Failed to parse image reference.
    #[error("Invalid image reference '{0}': {1}")]
    InvalidReference(String, String),

    /// Registry authentication failed.
    #[error("Authentication failed for registry '{0}': {1}")]
    AuthenticationFailed(String, String),

    /// Image or tag not found.
    #[error("Image not found: {0}")]
    ImageNotFound(String),

    /// Platform not available for image.
    #[error("Platform '{platform}' not available for image '{image}'")]
    PlatformNotAvailable {
        /// The image reference.
        image: String,
        /// The requested platform.
        platform: String,
    },

    /// Failed to pull blob from registry.
    #[error("Failed to pull blob {digest}: {message}")]
    BlobPullFailed {
        /// The blob digest.
        digest: String,
        /// Error message.
        message: String,
    },

    /// Failed to extract binary from archive.
    #[error("Failed to extract binary '{binary}' from archive: {message}")]
    ExtractionFailed {
        /// The binary name.
        binary: String,
        /// Error message.
        message: String,
    },

    /// Binary not found in archive.
    #[error("Binary '{0}' not found in archive")]
    BinaryNotFound(String),

    /// Cache operation failed.
    #[error("Cache error: {0}")]
    CacheError(String),

    /// IO error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// OCI distribution error.
    #[error("OCI error: {0}")]
    Oci(String),

    /// JSON parsing error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Digest mismatch after download.
    #[error("Digest mismatch for blob: expected {expected}, got {actual}")]
    DigestMismatch {
        /// The expected digest.
        expected: String,
        /// The computed digest.
        actual: String,
    },
}

impl Error {
    /// Create an invalid reference error.
    #[must_use]
    pub fn invalid_reference(reference: impl Into<String>, message: impl Into<String>) -> Self {
        Self::InvalidReference(reference.into(), message.into())
    }

    /// Create a platform not available error.
    #[must_use]
    pub fn platform_not_available(image: impl Into<String>, platform: impl Into<String>) -> Self {
        Self::PlatformNotAvailable {
            image: image.into(),
            platform: platform.into(),
        }
    }

    /// Create an extraction failed error.
    #[must_use]
    pub fn extraction_failed(binary: impl Into<String>, message: impl Into<String>) -> Self {
        Self::ExtractionFailed {
            binary: binary.into(),
            message: message.into(),
        }
    }

    /// Create a blob pull failed error.
    #[must_use]
    pub fn blob_pull_failed(digest: impl Into<String>, message: impl Into<String>) -> Self {
        Self::BlobPullFailed {
            digest: digest.into(),
            message: message.into(),
        }
    }

    /// Create a digest mismatch error.
    #[must_use]
    pub fn digest_mismatch(expected: impl Into<String>, actual: impl Into<String>) -> Self {
        Self::DigestMismatch {
            expected: expected.into(),
            actual: actual.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalid_reference_error() {
        let err = Error::invalid_reference("nginx:latest", "invalid syntax");
        let msg = err.to_string();
        assert!(msg.contains("nginx:latest"));
        assert!(msg.contains("invalid syntax"));
    }

    #[test]
    fn test_platform_not_available_error() {
        let err = Error::platform_not_available("nginx:latest", "windows-arm64");
        let msg = err.to_string();
        assert!(msg.contains("windows-arm64"));
        assert!(msg.contains("nginx:latest"));
    }

    #[test]
    fn test_extraction_failed_error() {
        let err = Error::extraction_failed("nginx", "file not found");
        let msg = err.to_string();
        assert!(msg.contains("nginx"));
        assert!(msg.contains("file not found"));
    }

    #[test]
    fn test_blob_pull_failed_error() {
        let err = Error::blob_pull_failed("sha256:abc123", "network timeout");
        let msg = err.to_string();
        assert!(msg.contains("sha256:abc123"));
        assert!(msg.contains("network timeout"));
    }

    #[test]
    fn test_digest_mismatch_error() {
        let err = Error::digest_mismatch("sha256:expected", "sha256:actual");
        let msg = err.to_string();
        assert!(msg.contains("expected"));
        assert!(msg.contains("actual"));
    }

    #[test]
    fn test_authentication_failed_error() {
        let err = Error::AuthenticationFailed("ghcr.io".to_string(), "invalid token".to_string());
        let msg = err.to_string();
        assert!(msg.contains("ghcr.io"));
        assert!(msg.contains("invalid token"));
    }

    #[test]
    fn test_image_not_found_error() {
        let err = Error::ImageNotFound("nginx:nonexistent".to_string());
        let msg = err.to_string();
        assert!(msg.contains("nginx:nonexistent"));
    }

    #[test]
    fn test_binary_not_found_error() {
        let err = Error::BinaryNotFound("/usr/bin/missing".to_string());
        let msg = err.to_string();
        assert!(msg.contains("/usr/bin/missing"));
    }

    #[test]
    fn test_cache_error() {
        let err = Error::CacheError("disk full".to_string());
        let msg = err.to_string();
        assert!(msg.contains("disk full"));
    }

    #[test]
    fn test_oci_error() {
        let err = Error::Oci("manifest not found".to_string());
        let msg = err.to_string();
        assert!(msg.contains("manifest not found"));
    }

    #[test]
    fn test_io_error_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err: Error = io_err.into();
        let msg = err.to_string();
        assert!(msg.contains("file missing") || msg.contains("IO error"));
    }

    #[test]
    fn test_json_error_conversion() {
        let json_str = "not valid json {";
        let json_err = serde_json::from_str::<serde_json::Value>(json_str).unwrap_err();
        let err: Error = json_err.into();
        let msg = err.to_string();
        assert!(msg.contains("JSON error"));
    }

    #[test]
    fn test_error_debug_impl() {
        let err = Error::invalid_reference("test", "reason");
        let debug = format!("{err:?}");
        assert!(debug.contains("InvalidReference"));
    }
}
