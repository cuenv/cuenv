//! Digest type and canonical hashing.
//!
//! A [`Digest`] names a blob by `(sha256, size)`. Structurally it mirrors the
//! Bazel Remote Execution API v2 `Digest` message so that the same value can
//! later be handed to a `bazel-remote-apis` gRPC client without conversion.

use crate::error::Result;
use crate::reapi::CanonicalMessage;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::fmt;

/// A content digest: hex-encoded SHA-256 plus the byte size of the content.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Digest {
    /// Lowercase hex SHA-256 of the content (64 characters).
    pub hash: String,
    /// Length of the content in bytes.
    pub size_bytes: u64,
}

impl Digest {
    /// Compute the digest of `bytes`.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let hash = hex::encode(Sha256::digest(bytes));
        Self {
            hash,
            size_bytes: bytes.len() as u64,
        }
    }

    /// Canonical `hash/size` form used in the Bazel RE API resource names.
    #[must_use]
    pub fn to_resource(&self) -> String {
        format!("{}/{}", self.hash, self.size_bytes)
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}/{}", self.hash, self.size_bytes)
    }
}

/// Serialize a value to its canonical REAPI protobuf encoding.
///
/// A digest only means something relative to an encoding, and REAPI defines a
/// blob's name as the SHA-256 of its **protobuf** serialization. Servers rely
/// on that: a CAS verifies `digest == sha256(bytes)` before accepting a blob,
/// and an action cache parses the `ActionResult` it is given. Encoding these
/// messages any other way would make cuenv's store unreadable to every REAPI
/// implementation.
///
/// Protobuf is not canonical on its own, so [`crate::reapi`] pins the orderings
/// REAPI requires before encoding.
///
/// # Errors
///
/// Returns [`Error::Serialization`](crate::error::Error::Serialization) if the
/// value cannot be represented in REAPI — in practice only a blob size beyond
/// `i64::MAX`.
pub fn canonical_bytes(value: &impl CanonicalMessage) -> Result<Vec<u8>> {
    value.to_canonical_bytes()
}

/// Compute a digest over a value's canonical REAPI encoding.
///
/// # Errors
///
/// Returns any error produced by [`canonical_bytes`].
pub fn digest_of(value: &impl CanonicalMessage) -> Result<Digest> {
    let bytes = canonical_bytes(value)?;
    Ok(Digest::of_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_of_empty_is_stable() {
        let d = Digest::of_bytes(b"");
        assert_eq!(d.size_bytes, 0);
        assert_eq!(
            d.hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn digest_of_hello_world() {
        let d = Digest::of_bytes(b"hello world");
        assert_eq!(d.size_bytes, 11);
        assert_eq!(
            d.hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn display_and_resource_forms() {
        let d = Digest::of_bytes(b"x");
        assert!(d.to_string().starts_with("sha256:"));
        assert!(d.to_resource().contains('/'));
    }

    #[test]
    fn digest_of_round_trips_through_serde() {
        let d = Digest::of_bytes(b"payload");
        let json = serde_json::to_string(&d).unwrap();
        let back: Digest = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }
}
