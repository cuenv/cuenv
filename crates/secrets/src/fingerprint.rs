//! Secret fingerprinting for cache keys.
//!
//! A cache key has to change when a secret's value changes, or a rotated
//! credential keeps serving results produced with the old one. It must also
//! never carry the value itself: the key is written into the
//! content-addressed store as part of the `Command` message, and with a
//! remote cache that blob leaves the machine.
//!
//! A keyed hash satisfies both. The salt is the key, supplied out of band via
//! `CUENV_SECRET_SALT`, so a fingerprint is stable for a given deployment,
//! changes when the secret changes, and cannot be reversed by anyone who does
//! not hold the salt.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Prefix marking a value in a cache key as a secret fingerprint rather than
/// a literal. It makes a leaked-looking blob self-describing when someone
/// inspects the CAS.
pub const FINGERPRINT_PREFIX: &str = "cuenv-secret-fp:";

/// Compute the HMAC-SHA256 fingerprint of a secret, keyed by `salt`.
///
/// The name is bound in alongside the value so that the same secret exposed
/// under two names produces two fingerprints, and so that swapping which
/// variable holds which value changes the key.
///
/// Returns the value prefixed with [`FINGERPRINT_PREFIX`].
#[must_use]
pub fn compute_secret_fingerprint(name: &str, value: &str, salt: &str) -> String {
    // HMAC accepts a key of any length, so this cannot fail.
    let mut mac = <HmacSha256 as Mac>::new_from_slice(salt.as_bytes())
        .unwrap_or_else(|_| unreachable!("HMAC accepts keys of any length"));
    // Bind the name with an explicit length prefix. Concatenating `name` and
    // `value` directly would let ("AB", "C") and ("A", "BC") collide.
    mac.update(&(name.len() as u64).to_be_bytes());
    mac.update(name.as_bytes());
    mac.update(value.as_bytes());
    format!("{FINGERPRINT_PREFIX}{}", hex::encode(mac.finalize().into_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_deterministic() {
        assert_eq!(
            compute_secret_fingerprint("API_KEY", "secret123", "salt"),
            compute_secret_fingerprint("API_KEY", "secret123", "salt")
        );
    }

    #[test]
    fn fingerprint_changes_with_value() {
        // A rotated credential must not keep serving the old result.
        assert_ne!(
            compute_secret_fingerprint("API_KEY", "secret123", "salt"),
            compute_secret_fingerprint("API_KEY", "secret456", "salt")
        );
    }

    #[test]
    fn fingerprint_changes_with_salt() {
        assert_ne!(
            compute_secret_fingerprint("API_KEY", "secret123", "salt1"),
            compute_secret_fingerprint("API_KEY", "secret123", "salt2")
        );
    }

    #[test]
    fn fingerprint_changes_with_name() {
        assert_ne!(
            compute_secret_fingerprint("API_KEY", "secret123", "salt"),
            compute_secret_fingerprint("DB_PASSWORD", "secret123", "salt")
        );
    }

    #[test]
    fn name_and_value_cannot_be_confused_for_one_another() {
        // Without a length prefix these two would hash identical bytes.
        assert_ne!(
            compute_secret_fingerprint("AB", "C", "salt"),
            compute_secret_fingerprint("A", "BC", "salt")
        );
    }

    #[test]
    fn fingerprint_never_contains_the_secret() {
        let fingerprint = compute_secret_fingerprint("API_KEY", "hunter2", "salt");
        assert!(!fingerprint.contains("hunter2"));
        assert!(fingerprint.starts_with(FINGERPRINT_PREFIX));
    }

    #[test]
    fn fingerprint_is_a_full_sha256_in_hex() {
        let fingerprint = compute_secret_fingerprint("A", "B", "salt");
        let hex = fingerprint.strip_prefix(FINGERPRINT_PREFIX).unwrap();
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
