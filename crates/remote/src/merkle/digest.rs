//! Content-addressed digest type for REAPI

use crate::error::{RemoteError, Result};
use sha2::{Digest as Sha2Digest, Sha256};
use std::fmt;

/// A content-addressed digest (SHA256 hash + size)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Digest {
    /// SHA256 hash in hex format
    pub hash: String,

    /// Size of the content in bytes
    pub size_bytes: i64,
}

impl Digest {
    /// Create a new digest from hash and size
    pub fn new(hash: impl Into<String>, size_bytes: i64) -> Result<Self> {
        let hash = hash.into();

        // Validate hash is 64 hex characters (SHA256)
        if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(RemoteError::invalid_digest(format!(
                "Invalid SHA256 hash: expected 64 hex characters, got {}",
                hash
            )));
        }

        Ok(Self { hash, size_bytes })
    }

    /// Compute digest from bytes
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let hash = format!("{:x}", hasher.finalize());

        Self {
            hash,
            size_bytes: bytes.len() as i64,
        }
    }

    /// Parse a digest from "hash/size" format
    pub fn parse(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split('/').collect();
        if parts.len() != 2 {
            return Err(RemoteError::invalid_digest(format!(
                "Invalid digest format: expected 'hash/size', got '{}'",
                s
            )));
        }

        let hash = parts[0].to_string();
        let size_bytes: i64 = parts[1].parse().map_err(|_| {
            RemoteError::invalid_digest(format!("Invalid size in digest: {}", parts[1]))
        })?;

        Self::new(hash, size_bytes)
    }

    /// Check if this is an empty digest
    pub fn is_empty(&self) -> bool {
        self.size_bytes == 0
    }

    /// Get the digest as a string in "hash/size" format
    pub fn to_string_format(&self) -> String {
        format!("{}/{}", self.hash, self.size_bytes)
    }

    /// Convert to REAPI proto Digest
    #[must_use]
    pub fn to_proto(&self) -> crate::reapi::Digest {
        crate::reapi::Digest {
            hash: self.hash.clone(),
            size_bytes: self.size_bytes,
        }
    }

    /// Get the hash string
    #[must_use]
    pub fn hash(&self) -> &str {
        &self.hash
    }

    /// Get the size in bytes
    #[must_use]
    pub fn size(&self) -> i64 {
        self.size_bytes
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.hash, self.size_bytes)
    }
}

/// Empty digest constant (SHA256 of empty string)
pub const EMPTY_DIGEST: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855/0";

impl Default for Digest {
    fn default() -> Self {
        Self::parse(EMPTY_DIGEST).expect("empty digest should be valid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_bytes() {
        let data = b"hello world";
        let digest = Digest::from_bytes(data);

        assert_eq!(digest.size_bytes, 11);
        assert_eq!(digest.hash.len(), 64);
        // SHA256 of "hello world"
        assert_eq!(
            digest.hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_new_valid() {
        let hash = "a".repeat(64);
        let result = Digest::new(hash.clone(), 100);
        assert!(result.is_ok());

        let digest = result.unwrap();
        assert_eq!(digest.hash, hash);
        assert_eq!(digest.size_bytes, 100);
    }

    #[test]
    fn test_new_invalid_length() {
        let hash = "a".repeat(32); // Too short
        let result = Digest::new(hash, 100);
        assert!(result.is_err());
    }

    #[test]
    fn test_new_invalid_chars() {
        let mut hash = "a".repeat(63);
        hash.push('g'); // Invalid hex char
        let result = Digest::new(hash, 100);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_valid() {
        let s = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9/11";
        let result = Digest::parse(s);
        assert!(result.is_ok());

        let digest = result.unwrap();
        assert_eq!(
            digest.hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        assert_eq!(digest.size_bytes, 11);
    }

    #[test]
    fn test_parse_invalid() {
        assert!(Digest::parse("invalid").is_err());
        assert!(Digest::parse("hash/notanumber").is_err());
    }

    #[test]
    fn test_display() {
        let digest = Digest::from_bytes(b"hello");
        let s = digest.to_string();
        assert!(s.contains('/'));
        assert_eq!(s.split('/').count(), 2);
    }

    #[test]
    fn test_empty_digest() {
        let digest = Digest::default();
        assert!(digest.is_empty());
        assert_eq!(digest.size_bytes, 0);
    }
}
