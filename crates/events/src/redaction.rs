//! Global secret redaction for cuenv events.
//!
//! Provides a centralized registry for secrets that should be redacted from all output.
//! Secrets are registered at runtime and automatically applied to event content
//! before events reach renderers.

use std::collections::HashSet;
use std::sync::{LazyLock, RwLock};

/// Minimum secret length to redact (shorter secrets may cause false positives)
pub const MIN_SECRET_LENGTH: usize = 4;

/// Placeholder for redacted secrets
pub const REDACTED_PLACEHOLDER: &str = "*_*";

/// Global registry of secrets to redact
static SECRET_REGISTRY: LazyLock<RwLock<HashSet<String>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));

/// Register a secret value for redaction.
///
/// All future events will have this secret redacted from their content.
/// Secrets shorter than `MIN_SECRET_LENGTH` are ignored (too many false positives).
///
/// # Example
///
/// ```rust
/// use cuenv_events::redaction::register_secret;
///
/// // Register a secret for redaction
/// register_secret("my-secret-token-12345");
/// ```
pub fn register_secret(secret: impl Into<String>) {
    let secret = secret.into();
    if secret.len() >= MIN_SECRET_LENGTH
        && let Ok(mut registry) = SECRET_REGISTRY.write()
    {
        registry.insert(secret);
    }
}

/// Register multiple secrets at once.
///
/// More efficient than calling `register_secret` multiple times.
///
/// # Example
///
/// ```rust
/// use cuenv_events::redaction::register_secrets;
///
/// register_secrets(["secret1", "secret2", "secret3"]);
/// ```
pub fn register_secrets(secrets: impl IntoIterator<Item = impl Into<String>>) {
    if let Ok(mut registry) = SECRET_REGISTRY.write() {
        for secret in secrets {
            let s = secret.into();
            if s.len() >= MIN_SECRET_LENGTH {
                registry.insert(s);
            }
        }
    }
}

/// Redact all registered secrets from a string.
///
/// Returns the input with all registered secrets replaced with `*_*`.
/// Uses greedy matching (longer secrets are replaced first) to handle
/// overlapping secrets correctly.
///
/// # Example
///
/// ```rust
/// use cuenv_events::redaction::{register_secret, redact};
///
/// register_secret("password123");
/// let redacted = redact("The password is password123");
/// assert!(redacted.contains("*_*"));
/// ```
#[must_use]
pub fn redact(input: &str) -> String {
    let secrets = match SECRET_REGISTRY.read() {
        Ok(registry) => registry.clone(),
        Err(_) => return input.to_string(),
    };

    if secrets.is_empty() {
        return input.to_string();
    }

    // Sort by length descending for greedy matching (longer secrets first)
    let mut sorted: Vec<_> = secrets.into_iter().collect();
    sorted.sort_by_key(|s| std::cmp::Reverse(s.len()));

    let mut result = input.to_string();
    for secret in &sorted {
        result = result.replace(secret, REDACTED_PLACEHOLDER);
    }
    result
}

/// Check if any secrets are registered.
#[must_use]
pub fn has_secrets() -> bool {
    SECRET_REGISTRY
        .read()
        .map(|r| !r.is_empty())
        .unwrap_or(false)
}

/// Get the number of registered secrets.
#[must_use]
pub fn secret_count() -> usize {
    SECRET_REGISTRY.read().map(|r| r.len()).unwrap_or(0)
}

/// Clear all registered secrets.
///
/// This is primarily useful for testing to ensure test isolation.
#[cfg(test)]
pub fn clear_secrets() {
    if let Ok(mut registry) = SECRET_REGISTRY.write() {
        registry.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Use a mutex to ensure tests don't interfere with each other
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn with_clean_registry<F, R>(f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_secrets();
        let result = f();
        clear_secrets();
        result
    }

    #[test]
    fn test_simple_redaction() {
        with_clean_registry(|| {
            register_secret("secret123");
            let result = redact("The password is secret123, don't share it");
            assert_eq!(result, "The password is *_*, don't share it");
        });
    }

    #[test]
    fn test_multiple_secrets() {
        with_clean_registry(|| {
            register_secrets(["password123", "api_key_xyz"]);
            let result = redact("password123 and api_key_xyz are both secrets");
            assert_eq!(result, "*_* and *_* are both secrets");
        });
    }

    #[test]
    fn test_repeated_secret() {
        with_clean_registry(|| {
            register_secret("secret");
            let result = redact("secret appears twice: secret");
            assert_eq!(result, "*_* appears twice: *_*");
        });
    }

    #[test]
    fn test_short_secret_ignored() {
        with_clean_registry(|| {
            register_secret("ab"); // Too short (< 4 chars)
            register_secret("abc"); // Too short
            register_secret("abcd"); // Just right (= 4 chars)

            assert_eq!(secret_count(), 1);

            let result = redact("ab abc abcd");
            assert_eq!(result, "ab abc *_*");
        });
    }

    #[test]
    fn test_empty_input() {
        with_clean_registry(|| {
            register_secret("secret");
            let result = redact("");
            assert_eq!(result, "");
        });
    }

    #[test]
    fn test_no_secrets_registered() {
        with_clean_registry(|| {
            assert!(!has_secrets());
            let result = redact("nothing to redact here");
            assert_eq!(result, "nothing to redact here");
        });
    }

    #[test]
    fn test_greedy_matching() {
        with_clean_registry(|| {
            // Longer secret should be matched first
            register_secrets(["pass", "password"]);
            let result = redact("the password is set");
            // Should redact "password" not just "pass"
            assert_eq!(result, "the *_* is set");
        });
    }

    #[test]
    fn test_secret_at_boundaries() {
        with_clean_registry(|| {
            register_secret("secret");

            // Secret at start
            let result = redact("secret is here");
            assert_eq!(result, "*_* is here");

            // Secret at end
            let result = redact("here is secret");
            assert_eq!(result, "here is *_*");
        });
    }

    #[test]
    fn test_special_characters() {
        with_clean_registry(|| {
            register_secret("pass$word!@#");
            let result = redact("the pass$word!@# is special");
            assert_eq!(result, "the *_* is special");
        });
    }

    #[test]
    fn test_multiline_content() {
        with_clean_registry(|| {
            register_secret("secretkey");
            let input = "line1\nsecretkey\nline3";
            let result = redact(input);
            assert_eq!(result, "line1\n*_*\nline3");
        });
    }

    #[test]
    fn test_has_secrets() {
        with_clean_registry(|| {
            assert!(!has_secrets());
            register_secret("test_secret");
            assert!(has_secrets());
        });
    }

    #[test]
    fn test_secret_count() {
        with_clean_registry(|| {
            assert_eq!(secret_count(), 0);
            register_secret("secret1");
            assert_eq!(secret_count(), 1);
            register_secret("secret2");
            assert_eq!(secret_count(), 2);
            // Duplicate should not increase count
            register_secret("secret1");
            assert_eq!(secret_count(), 2);
        });
    }
}
