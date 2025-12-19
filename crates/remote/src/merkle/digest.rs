use serde::{Deserialize, Serialize};
use sha2::{Digest as Sha2Digest, Sha256};
use std::fmt;

/// A Content Addressable Storage (CAS) digest, consisting of a SHA256 hash and size in bytes.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Digest {
    pub hash: String,
    pub size_bytes: i64,
}

impl Digest {
    /// Creates a new Digest from a hash string and size.
    pub fn new(hash: String, size_bytes: i64) -> Self {
        Self { hash, size_bytes }
    }

    /// Creates a Digest from the given content.
    pub fn from_content(content: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(content);
        let result = hasher.finalize();
        let hash = hex::encode(result);
        let size_bytes = content.len() as i64;
        Self { hash, size_bytes }
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.hash, self.size_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_digest_creation() {
        let d = Digest::new("abc".to_string(), 123);
        assert_eq!(d.hash, "abc");
        assert_eq!(d.size_bytes, 123);
    }

    #[test]
    fn test_digest_debug_fmt() {
        let d = Digest::new("abc".to_string(), 123);
        assert_eq!(format!("{:?}", d), "abc/123");
    }
    
    // Test that should fail initially because I haven't implemented the hashing logic yet
    #[test]
    fn test_from_content() {
        let content = b"hello world";
        let d = Digest::from_content(content);
        // SHA256 of "hello world" is b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9
        assert_eq!(d.hash, "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9");
        assert_eq!(d.size_bytes, 11);
    }
}
