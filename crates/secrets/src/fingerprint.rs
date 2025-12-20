//! Secret fingerprinting for cache keys

use sha2::{Digest, Sha256};

/// Compute HMAC-SHA256 fingerprint for a secret
///
/// Uses HMAC construction: H(salt || name || value)
/// This prevents rainbow table attacks on common secret values
#[must_use]
pub fn compute_secret_fingerprint(name: &str, value: &str, salt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(name.as_bytes());
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fingerprint_deterministic() {
        let fp1 = compute_secret_fingerprint("API_KEY", "secret123", "salt");
        let fp2 = compute_secret_fingerprint("API_KEY", "secret123", "salt");
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn test_fingerprint_changes_with_value() {
        let fp1 = compute_secret_fingerprint("API_KEY", "secret123", "salt");
        let fp2 = compute_secret_fingerprint("API_KEY", "secret456", "salt");
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn test_fingerprint_changes_with_salt() {
        let fp1 = compute_secret_fingerprint("API_KEY", "secret123", "salt1");
        let fp2 = compute_secret_fingerprint("API_KEY", "secret123", "salt2");
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn test_fingerprint_changes_with_name() {
        let fp1 = compute_secret_fingerprint("API_KEY", "secret123", "salt");
        let fp2 = compute_secret_fingerprint("DB_PASSWORD", "secret123", "salt");
        assert_ne!(fp1, fp2);
    }
}
