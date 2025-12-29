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
