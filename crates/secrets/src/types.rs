//! Secure secret types with automatic memory zeroing
//!
//! This module provides types for handling secrets securely in memory:
//! - [`SecureSecret`]: A wrapper around `secrecy::SecretString` that auto-zeros on drop
//! - [`BatchSecrets`]: A collection of resolved secrets with per-batch lifetime

use secrecy::{ExposeSecret, SecretString};
use std::collections::HashMap;

/// A resolved secret value with automatic memory zeroing on drop.
///
/// This type wraps `secrecy::SecretString` to ensure:
/// - Secret values are zeroed from memory when dropped
/// - Debug output shows `[REDACTED]` instead of the actual value
/// - Explicit `.expose()` call required to access the value
///
/// # Example
///
/// ```ignore
/// let secret = SecureSecret::new("my-password".to_string());
/// // Use the secret
/// let value = secret.expose();
/// // When `secret` goes out of scope, memory is zeroed
/// ```
#[derive(Clone)]
pub struct SecureSecret {
    inner: SecretString,
}

impl SecureSecret {
    /// Create a new secure secret from a string.
    ///
    /// The string value is moved into secure storage and will be
    /// automatically zeroed when this `SecureSecret` is dropped.
    #[must_use]
    pub fn new(value: String) -> Self {
        Self {
            inner: SecretString::from(value),
        }
    }

    /// Expose the secret value for use.
    ///
    /// # Safety Note
    ///
    /// The caller must ensure the exposed value is:
    /// - Not logged or printed
    /// - Not persisted to disk
    /// - Used only for the immediate operation (e.g., setting an env var)
    #[must_use]
    pub fn expose(&self) -> &str {
        self.inner.expose_secret()
    }

    /// Get the length of the secret value without exposing it.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.expose_secret().len()
    }

    /// Check if the secret value is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.expose_secret().is_empty()
    }
}

impl std::fmt::Debug for SecureSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl std::fmt::Display for SecureSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// Batch of resolved secrets with per-batch lifetime.
///
/// Secrets are automatically zeroed when this struct is dropped.
/// This provides secure handling for secrets resolved ahead of task execution.
///
/// # Lifetime
///
/// This struct is designed for per-batch use:
/// 1. Resolve all secrets needed for a batch of tasks
/// 2. Use the secrets during task execution
/// 3. Drop the `BatchSecrets` when the batch completes
/// 4. Memory is automatically zeroed on drop
///
/// # Example
///
/// ```ignore
/// let mut batch = BatchSecrets::new();
/// batch.insert("API_KEY".to_string(), SecureSecret::new("secret".to_string()), None);
///
/// // Use during task execution
/// if let Some(secret) = batch.get("API_KEY") {
///     std::env::set_var("API_KEY", secret.expose());
/// }
///
/// // When batch goes out of scope, all secrets are zeroed
/// ```
#[derive(Default)]
pub struct BatchSecrets {
    /// Secret name -> secure value
    secrets: HashMap<String, SecureSecret>,
    /// Secret name -> HMAC fingerprint (for cache keys)
    fingerprints: HashMap<String, String>,
}

impl BatchSecrets {
    /// Create an empty batch.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a batch with pre-allocated capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            secrets: HashMap::with_capacity(capacity),
            fingerprints: HashMap::with_capacity(capacity),
        }
    }

    /// Insert a secret into the batch.
    ///
    /// # Arguments
    ///
    /// * `name` - The secret name/key
    /// * `value` - The secure secret value
    /// * `fingerprint` - Optional HMAC fingerprint for cache key inclusion
    pub fn insert(&mut self, name: String, value: SecureSecret, fingerprint: Option<String>) {
        if let Some(fp) = fingerprint {
            self.fingerprints.insert(name.clone(), fp);
        }
        self.secrets.insert(name, value);
    }

    /// Get a secret by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&SecureSecret> {
        self.secrets.get(name)
    }

    /// Check if the batch contains a secret.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.secrets.contains_key(name)
    }

    /// Check if the batch is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    /// Get the number of secrets in the batch.
    #[must_use]
    pub fn len(&self) -> usize {
        self.secrets.len()
    }

    /// Get the fingerprints map.
    #[must_use]
    pub fn fingerprints(&self) -> &HashMap<String, String> {
        &self.fingerprints
    }

    /// Get the fingerprint for a specific secret.
    #[must_use]
    pub fn fingerprint(&self, name: &str) -> Option<&str> {
        self.fingerprints.get(name).map(String::as_str)
    }

    /// Iterate over secret names.
    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.secrets.keys()
    }

    /// Convert to environment variable map for process injection.
    ///
    /// # Warning
    ///
    /// This exposes all secret values. Use carefully and ensure the
    /// resulting map is not logged or persisted.
    #[must_use]
    pub fn into_env_map(self) -> HashMap<String, String> {
        self.secrets
            .into_iter()
            .map(|(k, v)| (k, v.expose().to_string()))
            .collect()
    }

    /// Convert to `ResolvedSecrets` for backward compatibility.
    ///
    /// This consumes the batch and converts it to the legacy format.
    #[must_use]
    pub fn into_resolved_secrets(self) -> crate::ResolvedSecrets {
        let fingerprints = self.fingerprints;
        let values = self
            .secrets
            .into_iter()
            .map(|(k, v)| (k, v.expose().to_string()))
            .collect();
        crate::ResolvedSecrets {
            values,
            fingerprints,
        }
    }

    /// Merge another batch into this one.
    ///
    /// Secrets from `other` will overwrite existing secrets with the same name.
    pub fn merge(&mut self, other: Self) {
        for (name, value) in other.secrets {
            let fingerprint = other.fingerprints.get(&name).cloned();
            self.insert(name, value, fingerprint);
        }
    }
}

impl std::fmt::Debug for BatchSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatchSecrets")
            .field("count", &self.secrets.len())
            .field("names", &self.secrets.keys().collect::<Vec<_>>())
            .field("fingerprints", &self.fingerprints.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure_secret_debug_is_redacted() {
        let secret = SecureSecret::new("my-super-secret-password".to_string());
        let debug_output = format!("{secret:?}");
        assert_eq!(debug_output, "[REDACTED]");
        assert!(!debug_output.contains("password"));
    }

    #[test]
    fn secure_secret_display_is_redacted() {
        let secret = SecureSecret::new("my-super-secret-password".to_string());
        let display_output = format!("{secret}");
        assert_eq!(display_output, "[REDACTED]");
    }

    #[test]
    fn secure_secret_expose_returns_value() {
        let secret = SecureSecret::new("test-value".to_string());
        assert_eq!(secret.expose(), "test-value");
    }

    #[test]
    fn secure_secret_len_works() {
        let secret = SecureSecret::new("12345".to_string());
        assert_eq!(secret.len(), 5);
        assert!(!secret.is_empty());
    }

    #[test]
    fn batch_secrets_insert_and_get() {
        let mut batch = BatchSecrets::new();
        batch.insert(
            "API_KEY".to_string(),
            SecureSecret::new("secret123".to_string()),
            Some("fingerprint123".to_string()),
        );

        assert!(batch.contains("API_KEY"));
        assert!(!batch.contains("OTHER"));
        assert_eq!(batch.len(), 1);

        let secret = batch.get("API_KEY").unwrap();
        assert_eq!(secret.expose(), "secret123");
        assert_eq!(batch.fingerprint("API_KEY"), Some("fingerprint123"));
    }

    #[test]
    fn batch_secrets_debug_hides_values() {
        let mut batch = BatchSecrets::new();
        batch.insert(
            "SECRET".to_string(),
            SecureSecret::new("password".to_string()),
            None,
        );

        let debug_output = format!("{batch:?}");
        assert!(!debug_output.contains("password"));
        assert!(debug_output.contains("SECRET"));
        assert!(debug_output.contains("count"));
    }

    #[test]
    fn batch_secrets_into_env_map() {
        let mut batch = BatchSecrets::new();
        batch.insert(
            "KEY1".to_string(),
            SecureSecret::new("value1".to_string()),
            None,
        );
        batch.insert(
            "KEY2".to_string(),
            SecureSecret::new("value2".to_string()),
            None,
        );

        let env_map = batch.into_env_map();
        assert_eq!(env_map.get("KEY1"), Some(&"value1".to_string()));
        assert_eq!(env_map.get("KEY2"), Some(&"value2".to_string()));
    }

    #[test]
    fn batch_secrets_merge() {
        let mut batch1 = BatchSecrets::new();
        batch1.insert(
            "KEY1".to_string(),
            SecureSecret::new("value1".to_string()),
            None,
        );

        let mut batch2 = BatchSecrets::new();
        batch2.insert(
            "KEY2".to_string(),
            SecureSecret::new("value2".to_string()),
            Some("fp2".to_string()),
        );

        batch1.merge(batch2);

        assert_eq!(batch1.len(), 2);
        assert!(batch1.contains("KEY1"));
        assert!(batch1.contains("KEY2"));
        assert_eq!(batch1.fingerprint("KEY2"), Some("fp2"));
    }
}
